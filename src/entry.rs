use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use pyo3::prelude::*;

pub struct SieveEntry {
    pub value: Py<PyAny>,
    pub created_at: Instant,
    pub visited: AtomicBool,
}

impl Clone for SieveEntry {
    fn clone(&self) -> Self {
        Python::attach(|py| SieveEntry {
            value: self.value.clone_ref(py),
            created_at: self.created_at,
            visited: AtomicBool::new(self.visited.load(Ordering::Relaxed)),
        })
    }
}
