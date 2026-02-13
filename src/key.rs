use std::hash::{Hash, Hasher};

use pyo3::ffi;
use pyo3::prelude::*;

pub struct CacheKey {
    hash: isize,
    pub key_obj: Py<PyAny>,
}

impl CacheKey {
    #[inline(always)]
    pub fn new(py: Python<'_>, obj: Py<PyAny>) -> PyResult<Self> {
        let hash = obj.bind(py).hash()?;
        Ok(CacheKey { hash, key_obj: obj })
    }
}

impl Clone for CacheKey {
    #[inline(always)]
    fn clone(&self) -> Self {
        Python::attach(|py| CacheKey {
            hash: self.hash,
            key_obj: self.key_obj.clone_ref(py),
        })
    }
}

impl Hash for CacheKey {
    #[inline(always)]
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.hash.hash(state);
    }
}

impl PartialEq for CacheKey {
    #[inline(always)]
    fn eq(&self, other: &Self) -> bool {
        if self.hash != other.hash {
            return false;
        }
        // SAFETY: PartialEq is only called from HashMap lookups inside
        // #[pymethods] (__call__, cache_clear), so the GIL is always held.
        // This is the same direct C API call that lru_cache uses.
        unsafe {
            ffi::PyObject_RichCompareBool(self.key_obj.as_ptr(), other.key_obj.as_ptr(), ffi::Py_EQ)
                == 1
        }
    }
}

impl Eq for CacheKey {}
