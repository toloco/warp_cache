use pyo3::prelude::*;
use pyo3::types::{PyDict, PyTuple};

/// Stub cache info for Windows (shared backend not supported).
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
        "SharedCacheInfo(unavailable on Windows)".to_string()
    }
}

/// Stub SharedCachedFunction for Windows — constructor raises an error.
#[pyclass(frozen)]
pub struct SharedCachedFunction;

#[pymethods]
impl SharedCachedFunction {
    #[new]
    #[pyo3(signature = (_fn_obj, _strategy, _max_size, _ttl=None, _max_key_size=512, _max_value_size=4096, _shm_name=None))]
    fn new(
        _fn_obj: Py<PyAny>,
        _strategy: u8,
        _max_size: usize,
        _ttl: Option<f64>,
        _max_key_size: usize,
        _max_value_size: usize,
        _shm_name: Option<String>,
    ) -> PyResult<Self> {
        Err(pyo3::exceptions::PyRuntimeError::new_err(
            "SharedCachedFunction is not supported on Windows",
        ))
    }

    #[pyo3(signature = (*_args, **_kwargs))]
    fn __call__<'py>(
        &self,
        _py: Python<'py>,
        _args: Bound<'py, PyTuple>,
        _kwargs: Option<Bound<'py, PyDict>>,
    ) -> PyResult<Py<PyAny>> {
        Err(pyo3::exceptions::PyRuntimeError::new_err(
            "SharedCachedFunction is not supported on Windows",
        ))
    }

    #[pyo3(signature = (*_args, **_kwargs))]
    fn get<'py>(
        &self,
        _py: Python<'py>,
        _args: Bound<'py, PyTuple>,
        _kwargs: Option<Bound<'py, PyDict>>,
    ) -> PyResult<Option<Py<PyAny>>> {
        Err(pyo3::exceptions::PyRuntimeError::new_err(
            "SharedCachedFunction is not supported on Windows",
        ))
    }

    #[pyo3(signature = (_value, *_args, **_kwargs))]
    fn set<'py>(
        &self,
        _py: Python<'py>,
        _value: Py<PyAny>,
        _args: Bound<'py, PyTuple>,
        _kwargs: Option<Bound<'py, PyDict>>,
    ) -> PyResult<()> {
        Err(pyo3::exceptions::PyRuntimeError::new_err(
            "SharedCachedFunction is not supported on Windows",
        ))
    }

    fn cache_info(&self) -> PyResult<SharedCacheInfo> {
        Err(pyo3::exceptions::PyRuntimeError::new_err(
            "SharedCachedFunction is not supported on Windows",
        ))
    }

    fn cache_clear(&self) -> PyResult<()> {
        Err(pyo3::exceptions::PyRuntimeError::new_err(
            "SharedCachedFunction is not supported on Windows",
        ))
    }
}
