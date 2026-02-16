mod entry;
mod key;
mod store;
mod strategies;

#[cfg(not(target_os = "windows"))]
mod serde;
#[cfg(not(target_os = "windows"))]
mod shared_store;
#[cfg(not(target_os = "windows"))]
mod shm;

#[cfg(target_os = "windows")]
mod shared_store_stub;

use pyo3::prelude::*;
use store::{CacheInfo, CachedFunction};

#[cfg(not(target_os = "windows"))]
use shared_store::{SharedCacheInfo, SharedCachedFunction};

#[cfg(target_os = "windows")]
use shared_store_stub::{SharedCacheInfo, SharedCachedFunction};

#[pymodule]
fn _warp_cache_rs(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<CachedFunction>()?;
    m.add_class::<CacheInfo>()?;
    m.add_class::<SharedCachedFunction>()?;
    m.add_class::<SharedCacheInfo>()?;
    Ok(())
}
