use std::time::{Duration, Instant};

use parking_lot::RwLock;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyTuple};

use crate::entry::CacheEntry;
use crate::key::CacheKey;
use crate::strategies::fifo::FifoStrategy;
use crate::strategies::lfu::LfuStrategy;
use crate::strategies::lru::LruStrategy;
use crate::strategies::mru::MruStrategy;
use crate::strategies::StrategyEnum;

struct CacheStoreInner {
    strategy: StrategyEnum,
    ttl: Option<Duration>,
    hits: u64,
    misses: u64,
}

enum LookupResult {
    Hit(Py<PyAny>),
    Miss,
    Expired,
}

impl CacheStoreInner {
    #[inline(always)]
    fn lookup(&mut self, py: Python<'_>, key: &CacheKey) -> LookupResult {
        match self.strategy.get_mut(key) {
            Some(entry) => {
                if let Some(ttl) = self.ttl {
                    if entry.created_at.elapsed() > ttl {
                        return LookupResult::Expired;
                    }
                }
                LookupResult::Hit(entry.value.clone_ref(py))
            }
            None => LookupResult::Miss,
        }
    }

    #[inline(always)]
    fn get(&mut self, py: Python<'_>, key: &CacheKey) -> Option<Py<PyAny>> {
        match self.lookup(py, key) {
            LookupResult::Hit(val) => {
                self.hits += 1;
                Some(val)
            }
            LookupResult::Miss => {
                self.misses += 1;
                None
            }
            LookupResult::Expired => {
                self.strategy.remove(key);
                self.misses += 1;
                None
            }
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
                hits: 0,
                misses: 0,
            }),
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

        // Lookup in cache
        {
            let mut inner = self.inner.write();
            if let Some(val) = inner.get(py, &cache_key) {
                return Ok(val);
            }
        }

        // Cache miss: call the wrapped function (outside lock)
        let result = self.fn_obj.bind(py).call(args, kwargs.as_ref())?.unbind();

        // Insert into cache
        {
            let mut inner = self.inner.write();
            let entry = CacheEntry {
                value: result.clone_ref(py),
                created_at: Instant::now(),
                frequency: 0,
            };
            inner.strategy.insert(cache_key, entry);
        }

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
        let mut inner = self.inner.write();
        Ok(inner.get(py, &cache_key))
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
            hits: inner.hits,
            misses: inner.misses,
            max_size: inner.strategy.capacity(),
            current_size: inner.strategy.len(),
        }
    }

    fn cache_clear(&self) {
        let mut inner = self.inner.write();
        inner.strategy.clear();
        inner.hits = 0;
        inner.misses = 0;
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
