use pyo3::prelude::*;
use std::time::Instant;

pub struct CacheEntry {
    pub value: Py<PyAny>,
    pub created_at: Instant,
    pub frequency: u64,
}
