use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyTuple};

use crate::serde;
use crate::shm::{ShmCache, ShmGetResult};

/// Cache info for the shared backend, exposed to Python.
#[pyclass(frozen)]
pub struct SharedCacheInfo {
    #[pyo3(get)]
    pub hits: u64,
    #[pyo3(get)]
    pub misses: u64,
    #[pyo3(get)]
    pub max_size: usize,
    #[pyo3(get)]
    pub current_size: usize,
    #[pyo3(get)]
    pub oversize_skips: u64,
}

#[pymethods]
impl SharedCacheInfo {
    fn __repr__(&self) -> String {
        format!(
            "SharedCacheInfo(hits={}, misses={}, max_size={}, current_size={}, oversize_skips={})",
            self.hits, self.misses, self.max_size, self.current_size, self.oversize_skips
        )
    }
}

/// A cached function using the shared-memory (mmap) backend.
///
/// Parallel to `CachedFunction` but stores serialized bytes in shared
/// memory accessible across processes.
#[pyclass(frozen)]
pub struct SharedCachedFunction {
    fn_obj: Py<PyAny>,
    pickle_dumps: Py<PyAny>,
    pickle_loads: Py<PyAny>,
    cache: parking_lot::Mutex<ShmCache>,
}

#[pymethods]
impl SharedCachedFunction {
    #[new]
    #[pyo3(signature = (fn_obj, strategy, max_size, ttl=None, max_key_size=512, max_value_size=4096, shm_name=None))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        py: Python<'_>,
        fn_obj: Py<PyAny>,
        strategy: u8,
        max_size: usize,
        ttl: Option<f64>,
        max_key_size: usize,
        max_value_size: usize,
        shm_name: Option<String>,
    ) -> PyResult<Self> {
        let pickle = py.import("pickle")?;
        let pickle_dumps = pickle.getattr("dumps")?.unbind();
        let pickle_loads = pickle.getattr("loads")?.unbind();

        // Derive a deterministic name from the function
        let name = match shm_name {
            Some(n) => n,
            None => derive_shm_name(py, &fn_obj)?,
        };

        let cache = ShmCache::create_or_open(
            &name,
            strategy as u32,
            max_size as u32,
            max_key_size as u32,
            max_value_size as u32,
            ttl,
        )
        .map_err(|e| {
            pyo3::exceptions::PyOSError::new_err(format!("Failed to create shared cache: {e}"))
        })?;

        Ok(SharedCachedFunction {
            fn_obj,
            pickle_dumps,
            pickle_loads,
            cache: parking_lot::Mutex::new(cache),
        })
    }

    #[pyo3(signature = (*args, **kwargs))]
    fn __call__<'py>(
        &self,
        py: Python<'py>,
        args: Bound<'py, PyTuple>,
        kwargs: Option<Bound<'py, PyDict>>,
    ) -> PyResult<Py<PyAny>> {
        let (key_hash, key_bytes) = self.make_key(py, &args, &kwargs)?;

        // Check size limits
        {
            let cache = self.cache.lock();
            if key_bytes.len() > cache.info().max_size && cache.is_oversize(&key_bytes, &[]) {
                cache.record_oversize_skip();
                drop(cache);
                return self
                    .fn_obj
                    .bind(py)
                    .call(args, kwargs.as_ref())
                    .map(|r| r.unbind());
            }
        }

        // Lookup in shared cache
        let value_bytes: Option<Vec<u8>> = {
            let cache = self.cache.lock();
            match cache.get(key_hash, &key_bytes) {
                ShmGetResult::Hit(v) => Some(v),
                ShmGetResult::Miss => None,
            }
        };

        // On hit: deserialize and return
        if let Some(vb) = value_bytes {
            return self.deserialize_value(py, &vb);
        }

        // Cache miss: call the wrapped function
        let result = self.fn_obj.bind(py).call(args, kwargs.as_ref())?;

        self.store_result(py, key_hash, &key_bytes, &result)?;

        Ok(result.unbind())
    }

    /// Cache lookup only. Returns the cached value or None on miss.
    #[pyo3(signature = (*args, **kwargs))]
    fn get<'py>(
        &self,
        py: Python<'py>,
        args: Bound<'py, PyTuple>,
        kwargs: Option<Bound<'py, PyDict>>,
    ) -> PyResult<Option<Py<PyAny>>> {
        let (key_hash, key_bytes) = self.make_key(py, &args, &kwargs)?;

        let cache = self.cache.lock();
        match cache.get(key_hash, &key_bytes) {
            ShmGetResult::Hit(vb) => {
                let value = self.deserialize_value(py, &vb)?;
                Ok(Some(value))
            }
            ShmGetResult::Miss => Ok(None),
        }
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
        let (key_hash, key_bytes) = self.make_key(py, &args, &kwargs)?;
        let result = value.bind(py);
        self.store_result(py, key_hash, &key_bytes, result)?;
        Ok(())
    }

    fn cache_info(&self) -> SharedCacheInfo {
        let cache = self.cache.lock();
        let info = cache.info();
        SharedCacheInfo {
            hits: info.hits,
            misses: info.misses,
            max_size: info.max_size,
            current_size: info.current_size,
            oversize_skips: info.oversize_skips,
        }
    }

    fn cache_clear(&self) {
        let mut cache = self.cache.lock();
        cache.clear();
    }
}

impl SharedCachedFunction {
    /// Build (key_hash, serialized_key_bytes) from call args.
    fn make_key<'py>(
        &self,
        py: Python<'py>,
        args: &Bound<'py, PyTuple>,
        kwargs: &Option<Bound<'py, PyDict>>,
    ) -> PyResult<(u64, Vec<u8>)> {
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

        let key_hash = {
            let py_hash: isize = key_obj.bind(py).hash()?;
            let mut hasher = DefaultHasher::new();
            py_hash.hash(&mut hasher);
            hasher.finish()
        };

        // Fast path: serialize key without pickle
        let key_bound = key_obj.bind(py);
        if let Some(bytes) = serde::serialize(py, key_bound)? {
            return Ok((key_hash, bytes));
        }

        // Fallback: pickle
        let pickle_obj = self.pickle_dumps.bind(py).call1((&key_obj,))?;
        let pickle_bytes: &[u8] = pickle_obj.extract()?;
        Ok((key_hash, serde::wrap_pickle(pickle_bytes)))
    }

    /// Serialize and store a result, checking value size limits.
    fn store_result<'py>(
        &self,
        py: Python<'py>,
        key_hash: u64,
        key_bytes: &[u8],
        result: &Bound<'py, PyAny>,
    ) -> PyResult<()> {
        // Fast path: serialize value without pickle
        let value_bytes = if let Some(bytes) = serde::serialize(py, result)? {
            bytes
        } else {
            let pickle_obj = self.pickle_dumps.bind(py).call1((result,))?;
            let pickle_bytes: &[u8] = pickle_obj.extract()?;
            serde::wrap_pickle(pickle_bytes)
        };

        {
            let cache = self.cache.lock();
            if cache.is_oversize(key_bytes, &value_bytes) {
                cache.record_oversize_skip();
                return Ok(());
            }
        }

        {
            let mut cache = self.cache.lock();
            cache.insert(key_hash, key_bytes, &value_bytes);
        }
        Ok(())
    }

    /// Deserialize a value from shared memory bytes.
    fn deserialize_value(&self, py: Python, data: &[u8]) -> PyResult<Py<PyAny>> {
        // Fast path
        if let Some(obj) = serde::deserialize(py, data)? {
            return Ok(obj);
        }
        // Fallback: pickle (skip TAG_PICKLE byte)
        let payload = serde::pickle_payload(data);
        let value = self.pickle_loads.bind(py).call1((payload,))?;
        Ok(value.unbind())
    }
}

/// Derive a deterministic shared memory name from the function's module and qualname.
fn derive_shm_name(py: Python<'_>, fn_obj: &Py<PyAny>) -> PyResult<String> {
    let bound = fn_obj.bind(py);
    let module = bound
        .getattr("__module__")
        .and_then(|m| m.extract::<String>())
        .unwrap_or_else(|_| "unknown".to_string());
    let qualname = bound
        .getattr("__qualname__")
        .and_then(|q| q.extract::<String>())
        .unwrap_or_else(|_| "unknown".to_string());

    // Hash for uniqueness
    let mut hasher = DefaultHasher::new();
    module.hash(&mut hasher);
    qualname.hash(&mut hasher);
    let hash = hasher.finish();

    Ok(format!("warp_cache_{module}_{qualname}_{hash:016x}"))
}
