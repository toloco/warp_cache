use std::hash::{Hash, Hasher};

use pyo3::ffi;
use pyo3::prelude::*;

pub struct CacheKey {
    pub(crate) hash: isize,
    pub key_obj: Py<PyAny>,
}

impl CacheKey {
    #[inline(always)]
    pub fn new(py: Python<'_>, obj: Py<PyAny>) -> PyResult<Self> {
        let hash = obj.bind(py).hash()?;
        Ok(CacheKey { hash, key_obj: obj })
    }

    /// Build a CacheKey from a pre-computed hash and an already-owned object.
    #[inline(always)]
    pub fn with_hash(hash: isize, obj: Py<PyAny>) -> Self {
        CacheKey { hash, key_obj: obj }
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

impl CacheKey {
    #[inline(always)]
    pub fn shard_index(&self, shard_mask: usize) -> usize {
        self.hash as usize & shard_mask
    }
}

// ---------------------------------------------------------------------------
// BorrowedArgs — a zero-allocation key for read-path lookups.
//
// On a cache hit we never need to create a CacheKey (which clones the args
// tuple and bumps its refcount). Instead we borrow the raw pointer and
// pre-computed hash, look up through hashbrown's `Equivalent` trait, and
// only materialise a CacheKey on the miss path when we need to store it.
// ---------------------------------------------------------------------------

pub(crate) struct BorrowedArgs {
    pub hash: isize,
    pub ptr: *mut ffi::PyObject,
}

impl Hash for BorrowedArgs {
    #[inline(always)]
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.hash.hash(state);
    }
}

impl hashbrown::Equivalent<CacheKey> for BorrowedArgs {
    #[inline(always)]
    fn equivalent(&self, key: &CacheKey) -> bool {
        if self.hash != key.hash {
            return false;
        }
        // SAFETY: Called only inside #[pymethods] where the GIL is held.
        // `self.ptr` points to a live Python object (the args tuple on the
        // call stack) and `key.key_obj` is an owned reference in the map.
        unsafe {
            ffi::PyObject_RichCompareBool(self.ptr, key.key_obj.as_ptr(), ffi::Py_EQ) == 1
        }
    }
}
