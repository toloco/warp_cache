use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::{Mutex, RwLock};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyTuple};

use crate::entry::CacheEntry;
use crate::key::CacheKey;
use crate::strategies::fifo::FifoStrategy;
use crate::strategies::lfu::LfuStrategy;
use crate::strategies::lru::LruStrategy;
use crate::strategies::mru::MruStrategy;
use crate::strategies::StrategyEnum;

const ACCESS_LOG_CAPACITY: usize = 64;

struct CacheStoreInner {
    strategy: StrategyEnum,
    ttl: Option<Duration>,
}

impl CacheStoreInner {
    /// Drain the access log and replay deferred ordering updates.
    #[inline(always)]
    fn drain_access_log(&mut self, log: &mut Vec<CacheKey>) {
        for key in log.drain(..) {
            self.strategy.record_access(&key);
        }
    }
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
    inner: RwLock<CacheStoreInner>,
    access_log: Mutex<Vec<CacheKey>>,
    hits: AtomicU64,
    misses: AtomicU64,
}

#[pymethods]
impl CachedFunction {
    #[new]
    #[pyo3(signature = (fn_obj, strategy, max_size, ttl=None))]
    fn new(fn_obj: Py<PyAny>, strategy: u8, max_size: usize, ttl: Option<f64>) -> Self {
        let strat = match strategy {
            0 => StrategyEnum::Lru(LruStrategy::new(max_size)),
            1 => StrategyEnum::Mru(MruStrategy::new(max_size)),
            2 => StrategyEnum::Fifo(FifoStrategy::new(max_size)),
            3 => StrategyEnum::Lfu(LfuStrategy::new(max_size)),
            _ => StrategyEnum::Lru(LruStrategy::new(max_size)),
        };
        let ttl_dur = ttl.map(Duration::from_secs_f64);
        CachedFunction {
            fn_obj,
            inner: RwLock::new(CacheStoreInner {
                strategy: strat,
                ttl: ttl_dur,
            }),
            access_log: Mutex::new(Vec::with_capacity(ACCESS_LOG_CAPACITY)),
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
        // Build the cache key (inlined for performance on the hot path)
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

        // FAST PATH: read lock — cache hit
        {
            let inner = self.inner.read();
            if let Some(entry) = inner.strategy.peek(&cache_key) {
                if let Some(ttl) = inner.ttl {
                    if entry.created_at.elapsed() > ttl {
                        // Expired — fall through to slow path (can't remove under read lock)
                        drop(inner);
                    } else {
                        let val = entry.value.clone_ref(py);
                        drop(inner);
                        self.hits.fetch_add(1, Ordering::Relaxed);
                        let mut log = self.access_log.lock();
                        if log.len() < ACCESS_LOG_CAPACITY {
                            log.push(cache_key);
                        }
                        return Ok(val);
                    }
                } else {
                    let val = entry.value.clone_ref(py);
                    drop(inner);
                    self.hits.fetch_add(1, Ordering::Relaxed);
                    let mut log = self.access_log.lock();
                    if log.len() < ACCESS_LOG_CAPACITY {
                        log.push(cache_key);
                    }
                    return Ok(val);
                }
            }
        }

        // Cache miss: call the wrapped function (outside any lock)
        let result = self.fn_obj.bind(py).call(args, kwargs.as_ref())?.unbind();

        // SLOW PATH: write lock — drain access log + insert
        {
            let mut inner = self.inner.write();

            // Drain deferred access log
            let mut log = self.access_log.lock();
            inner.drain_access_log(&mut log);
            drop(log);

            // Double-check: another thread may have inserted while we were computing
            let needs_insert = match inner.strategy.peek(&cache_key) {
                Some(entry) => {
                    if let Some(ttl) = inner.ttl {
                        entry.created_at.elapsed() > ttl
                    } else {
                        false
                    }
                }
                None => true,
            };

            if needs_insert {
                // Remove expired entry if present
                inner.strategy.remove(&cache_key);
                let entry = CacheEntry {
                    value: result.clone_ref(py),
                    created_at: Instant::now(),
                    frequency: 0,
                };
                inner.strategy.insert(cache_key, entry);
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

        // FAST PATH: read lock
        {
            let inner = self.inner.read();
            if let Some(entry) = inner.strategy.peek(&cache_key) {
                if let Some(ttl) = inner.ttl {
                    if entry.created_at.elapsed() > ttl {
                        // Expired — need write lock to remove
                        drop(inner);
                    } else {
                        let val = entry.value.clone_ref(py);
                        drop(inner);
                        self.hits.fetch_add(1, Ordering::Relaxed);
                        let mut log = self.access_log.lock();
                        if log.len() < ACCESS_LOG_CAPACITY {
                            log.push(cache_key);
                        }
                        return Ok(Some(val));
                    }
                } else {
                    let val = entry.value.clone_ref(py);
                    drop(inner);
                    self.hits.fetch_add(1, Ordering::Relaxed);
                    let mut log = self.access_log.lock();
                    if log.len() < ACCESS_LOG_CAPACITY {
                        log.push(cache_key);
                    }
                    return Ok(Some(val));
                }
            }
        }

        // SLOW PATH: write lock for expired removal
        {
            let mut inner = self.inner.write();
            let mut log = self.access_log.lock();
            inner.drain_access_log(&mut log);
            drop(log);

            // Check again under write lock
            if let Some(entry) = inner.strategy.peek(&cache_key) {
                if let Some(ttl) = inner.ttl {
                    if entry.created_at.elapsed() > ttl {
                        inner.strategy.remove(&cache_key);
                        self.misses.fetch_add(1, Ordering::Relaxed);
                        return Ok(None);
                    }
                }
                // Hit (possibly inserted by another thread)
                let val = entry.value.clone_ref(py);
                inner.strategy.record_access(&cache_key);
                self.hits.fetch_add(1, Ordering::Relaxed);
                return Ok(Some(val));
            }
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
        let mut inner = self.inner.write();

        // Drain deferred access log
        let mut log = self.access_log.lock();
        inner.drain_access_log(&mut log);
        drop(log);

        let entry = CacheEntry {
            value: value.clone_ref(py),
            created_at: Instant::now(),
            frequency: 0,
        };
        inner.strategy.insert(cache_key, entry);
        Ok(())
    }

    fn cache_info(&self) -> CacheInfo {
        let inner = self.inner.read();
        CacheInfo {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            max_size: inner.strategy.capacity(),
            current_size: inner.strategy.len(),
        }
    }

    fn cache_clear(&self) {
        let mut inner = self.inner.write();
        inner.strategy.clear();
        self.access_log.lock().clear();
        self.hits.store(0, Ordering::Relaxed);
        self.misses.store(0, Ordering::Relaxed);
    }
}

impl CachedFunction {
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
