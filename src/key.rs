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
        // #[pymethods], so the GIL is always held. The arbitrary Python __eq__
        // this can run may re-enter the cache or hand off the GIL; CachedFunction
        // serializes that via its reentrancy guard (try_enter, see issue #30), so
        // no aliasing/reentrant shard guard is taken during this comparison.
        // This is the same direct C API call that lru_cache uses.
        unsafe { rich_compare_eq(self.key_obj.as_ptr(), other.key_obj.as_ptr()) }
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
        // call stack) and `key.key_obj` is an owned reference in the map. The
        // arbitrary Python __eq__ this runs may re-enter; CachedFunction's
        // reentrancy guard prevents a second, aliasing shard guard (issue #30).
        unsafe { rich_compare_eq(self.ptr, key.key_obj.as_ptr()) }
    }
}

/// `a == b` via Python's rich comparison, for use from `PartialEq`/`Equivalent`
/// (which can't return `Result`).
///
/// `PyObject_RichCompareBool` returns -1 and leaves a Python exception set when a
/// key's `__eq__` raises. We must NOT map that to `true`/`false` and silently drop
/// the exception (issue #36): callers fetch the pending exception after the lookup
/// and propagate it. Here, -1 is reported as "not equal" so hashbrown stops at this
/// slot, with the exception left set for the caller.
///
/// If an exception is already pending (an earlier comparison in the same lookup
/// raised), we return `false` without calling into Python again — re-entering the
/// interpreter with a live exception would clobber the original error.
///
/// # Safety
/// Both pointers must be valid live Python objects and the GIL must be held.
#[inline(always)]
unsafe fn rich_compare_eq(a: *mut ffi::PyObject, b: *mut ffi::PyObject) -> bool {
    if !ffi::PyErr_Occurred().is_null() {
        return false;
    }
    // 1 = equal; 0 = not equal; -1 = __eq__ raised (exception now set) — the latter
    // two are both "not equal" for the probe, but -1 leaves the error for the caller.
    ffi::PyObject_RichCompareBool(a, b, ffi::Py_EQ) == 1
}
