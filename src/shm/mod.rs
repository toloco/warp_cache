/// Shared-memory cache backend.
///
/// Provides `ShmCache` — a cross-process LRU/MRU/FIFO/LFU cache backed
/// by mmap. All data (header, hash table, slab arena) lives in a single
/// memory-mapped file. A separate mmap file holds the POSIX rwlock.
pub mod hashtable;
pub mod layout;
pub mod lock;
pub mod ordering;
pub mod region;

use layout::{Header, SlotHeader, SLOT_HEADER_SIZE, SLOT_NONE};
use lock::ShmRwLock;
use region::ShmRegion;

/// Result of a cache get operation.
pub enum ShmGetResult {
    Hit(Vec<u8>),
    Miss,
}

/// The main shared-memory cache handle.
///
/// One instance per decorated function. Multiple processes sharing the
/// same cache file will have independent `ShmCache` handles pointing
/// at the same mmap.
pub struct ShmCache {
    region: ShmRegion,
    next_unique_id: u64,
}

impl ShmCache {
    /// Create or open a shared cache.
    pub fn create_or_open(
        name: &str,
        strategy: u32,
        capacity: u32,
        max_key_size: u32,
        max_value_size: u32,
        ttl_secs: Option<f64>,
    ) -> std::io::Result<Self> {
        let slot_size = SLOT_HEADER_SIZE as u32 + max_key_size + max_value_size;
        let ttl_nanos = match ttl_secs {
            Some(t) => (t * 1_000_000_000.0) as u64,
            None => 0,
        };

        let region = ShmRegion::create_or_open(
            name,
            strategy,
            capacity,
            slot_size,
            max_key_size,
            max_value_size,
            ttl_nanos,
        )?;

        Ok(ShmCache {
            region,
            next_unique_id: 0,
        })
    }

    fn lock(&self) -> ShmRwLock {
        self.region.lock()
    }

    fn header(&self) -> &Header {
        self.region.header()
    }

    /// Get the mutable header pointer. Caller must hold write lock.
    unsafe fn header_mut(&self) -> &mut Header {
        &mut *(self.region.base_ptr() as *mut Header)
    }

    fn ht_base(&self) -> *const u8 {
        unsafe { self.region.base_ptr().add(layout::ht_offset()) }
    }

    fn ht_base_mut(&self) -> *mut u8 {
        unsafe { (self.region.base_ptr() as *mut u8).add(layout::ht_offset()) }
    }

    fn slab_base(&self) -> *const u8 {
        let ht_cap = self.header().ht_capacity;
        unsafe { self.region.base_ptr().add(layout::slab_offset(ht_cap)) }
    }

    fn slab_base_mut(&self) -> *mut u8 {
        let ht_cap = self.header().ht_capacity;
        unsafe { (self.region.base_ptr() as *mut u8).add(layout::slab_offset(ht_cap)) }
    }

    /// Check if key/value sizes exceed limits. Returns true if oversize.
    pub fn is_oversize(&self, key_bytes: &[u8], value_bytes: &[u8]) -> bool {
        let h = self.header();
        key_bytes.len() > h.max_key_size as usize || value_bytes.len() > h.max_value_size as usize
    }

    /// Look up a key (by hash + serialized bytes). Returns a copy of the value bytes on hit.
    ///
    /// This acquires a **write lock** so it can update ordering (LRU touch)
    /// and stats atomically.
    pub fn get(&self, key_hash: u64, key_bytes: &[u8]) -> ShmGetResult {
        let lock = self.lock();
        lock.write_lock();
        let result = unsafe { self.get_inner(key_hash, key_bytes) };
        lock.write_unlock();
        result
    }

    unsafe fn get_inner(&self, key_hash: u64, key_bytes: &[u8]) -> ShmGetResult {
        let h = self.header();
        let ht_cap = h.ht_capacity;
        let slot_size = h.slot_size;
        let strategy = h.strategy;
        let ttl_nanos = h.ttl_nanos;

        let slot_index = hashtable::ht_lookup(
            self.ht_base(),
            ht_cap,
            self.slab_base(),
            slot_size,
            key_hash,
            key_bytes,
        );

        match slot_index {
            Some(idx) => {
                let slot_ptr = self.slab_base().add(idx as usize * slot_size as usize);
                let slot = &*(slot_ptr as *const SlotHeader);

                // Check TTL
                if ttl_nanos > 0 {
                    let now = current_time_nanos();
                    if now.saturating_sub(slot.created_at_nanos) > ttl_nanos {
                        // Expired — remove and count as miss
                        self.remove_slot(idx, key_bytes);
                        let header = self.header_mut();
                        header.misses += 1;
                        return ShmGetResult::Miss;
                    }
                }

                // Copy value bytes out
                let value_offset = SLOT_HEADER_SIZE + slot.key_len as usize;
                let value_ptr = slot_ptr.add(value_offset);
                let value = std::slice::from_raw_parts(value_ptr, slot.value_len as usize).to_vec();

                // Update ordering and stats
                let header = self.header_mut();
                ordering::on_access(header, self.slab_base_mut(), slot_size, idx, strategy);
                header.hits += 1;

                ShmGetResult::Hit(value)
            }
            None => {
                let header = self.header_mut();
                header.misses += 1;
                ShmGetResult::Miss
            }
        }
    }

    /// Insert a key-value pair. Evicts if necessary.
    pub fn insert(&mut self, key_hash: u64, key_bytes: &[u8], value_bytes: &[u8]) {
        let lock = self.lock();
        lock.write_lock();
        unsafe { self.insert_inner(key_hash, key_bytes, value_bytes) };
        lock.write_unlock();
    }

    unsafe fn insert_inner(&mut self, key_hash: u64, key_bytes: &[u8], value_bytes: &[u8]) {
        let h = self.header();
        let ht_cap = h.ht_capacity;
        let slot_size = h.slot_size;
        let strategy = h.strategy;
        let capacity = h.capacity;

        // Check if key already exists — update value in place
        let existing = hashtable::ht_lookup(
            self.ht_base(),
            ht_cap,
            self.slab_base(),
            slot_size,
            key_hash,
            key_bytes,
        );

        if let Some(idx) = existing {
            // Update value in-place
            let slot_ptr = self.slab_base_mut().add(idx as usize * slot_size as usize);
            let slot = &mut *(slot_ptr as *mut SlotHeader);
            slot.value_len = value_bytes.len() as u32;
            slot.created_at_nanos = current_time_nanos();

            let value_dest = slot_ptr.add(SLOT_HEADER_SIZE + slot.key_len as usize);
            std::ptr::copy_nonoverlapping(value_bytes.as_ptr(), value_dest, value_bytes.len());

            let header = self.header_mut();
            ordering::on_access(header, self.slab_base_mut(), slot_size, idx, strategy);
            return;
        }

        // Allocate a slot
        let header = self.header_mut();
        let slot_idx = if header.free_head != SLOT_NONE {
            // Pop from free list
            let idx = header.free_head;
            let free_slot =
                &*(self.slab_base().add(idx as usize * slot_size as usize) as *const SlotHeader);
            header.free_head = free_slot.next;
            idx
        } else if header.current_size >= capacity {
            // Need to evict
            let evict_idx = ordering::evict_candidate(header, strategy);
            if evict_idx == SLOT_NONE {
                return; // shouldn't happen
            }

            // Remove evicted entry from hash table
            let evict_slot_ptr = self
                .slab_base()
                .add(evict_idx as usize * slot_size as usize);
            let evict_slot = &*(evict_slot_ptr as *const SlotHeader);
            let evict_key = std::slice::from_raw_parts(
                evict_slot_ptr.add(SLOT_HEADER_SIZE),
                evict_slot.key_len as usize,
            );

            hashtable::ht_remove(
                self.ht_base_mut(),
                ht_cap,
                self.slab_base(),
                slot_size,
                evict_slot.key_hash,
                evict_key,
            );

            ordering::list_remove(header, self.slab_base_mut(), slot_size, evict_idx);
            header.current_size -= 1;

            evict_idx
        } else {
            // This shouldn't happen if free list is properly maintained
            return;
        };

        // Write the new entry into the slot
        let slot_ptr = self
            .slab_base_mut()
            .add(slot_idx as usize * slot_size as usize);
        let slot = &mut *(slot_ptr as *mut SlotHeader);
        slot.occupied = 1;
        slot.key_hash = key_hash;
        slot.key_len = key_bytes.len() as u32;
        slot.value_len = value_bytes.len() as u32;
        slot.created_at_nanos = current_time_nanos();
        slot.frequency = 0;
        slot.prev = SLOT_NONE;
        slot.next = SLOT_NONE;
        slot.unique_id = self.next_unique_id;
        self.next_unique_id += 1;

        // Copy key bytes
        let key_dest = slot_ptr.add(SLOT_HEADER_SIZE);
        std::ptr::copy_nonoverlapping(key_bytes.as_ptr(), key_dest, key_bytes.len());

        // Copy value bytes
        let value_dest = key_dest.add(key_bytes.len());
        std::ptr::copy_nonoverlapping(value_bytes.as_ptr(), value_dest, value_bytes.len());

        // Insert into hash table
        hashtable::ht_insert(self.ht_base_mut(), ht_cap, key_hash, slot_idx);

        // Add to eviction list
        let header = self.header_mut();
        ordering::on_insert(header, self.slab_base_mut(), slot_size, slot_idx, strategy);
        header.current_size += 1;
    }

    /// Remove a specific slot.
    unsafe fn remove_slot(&self, slot_idx: i32, key_bytes: &[u8]) {
        let h = self.header();
        let ht_cap = h.ht_capacity;
        let slot_size = h.slot_size;

        let slot_ptr = self.slab_base().add(slot_idx as usize * slot_size as usize);
        let slot = &*(slot_ptr as *const SlotHeader);
        let key_hash = slot.key_hash;

        // Remove from hash table
        hashtable::ht_remove(
            self.ht_base_mut(),
            ht_cap,
            self.slab_base(),
            slot_size,
            key_hash,
            key_bytes,
        );

        // Remove from eviction list
        let header = self.header_mut();
        ordering::list_remove(header, self.slab_base_mut(), slot_size, slot_idx);

        // Mark slot as free and push to free list
        let slot = &mut *(self
            .slab_base_mut()
            .add(slot_idx as usize * slot_size as usize)
            as *mut SlotHeader);
        slot.occupied = 0;
        slot.next = header.free_head;
        slot.prev = SLOT_NONE;
        header.free_head = slot_idx;
        header.current_size -= 1;
    }

    /// Clear the entire cache.
    pub fn clear(&mut self) {
        let lock = self.lock();
        lock.write_lock();
        unsafe { self.clear_inner() };
        lock.write_unlock();
    }

    unsafe fn clear_inner(&mut self) {
        let h = self.header();
        let ht_cap = h.ht_capacity;
        let slot_size = h.slot_size;
        let capacity = h.capacity;

        // Clear hash table
        hashtable::ht_clear(self.ht_base_mut(), ht_cap);

        // Reset all slots to free list
        for i in 0..capacity as usize {
            let slot_ptr = self.slab_base_mut().add(i * slot_size as usize);
            let slot = &mut *(slot_ptr as *mut SlotHeader);
            slot.occupied = 0;
            slot.prev = SLOT_NONE;
            slot.next = if i + 1 < capacity as usize {
                (i + 1) as i32
            } else {
                SLOT_NONE
            };
        }

        let header = self.header_mut();
        header.current_size = 0;
        header.hits = 0;
        header.misses = 0;
        header.oversize_skips = 0;
        header.list_head = SLOT_NONE;
        header.list_tail = SLOT_NONE;
        header.free_head = 0;
    }

    /// Increment oversize skip counter.
    pub fn record_oversize_skip(&self) {
        let lock = self.lock();
        lock.write_lock();
        unsafe {
            self.header_mut().oversize_skips += 1;
        }
        lock.write_unlock();
    }

    /// Get cache statistics.
    pub fn info(&self) -> ShmCacheInfo {
        let lock = self.lock();
        lock.read_lock();
        let h = self.header();
        let info = ShmCacheInfo {
            hits: h.hits,
            misses: h.misses,
            max_size: h.capacity as usize,
            current_size: h.current_size as usize,
            oversize_skips: h.oversize_skips,
        };
        lock.read_unlock();
        info
    }
}

pub struct ShmCacheInfo {
    pub hits: u64,
    pub misses: u64,
    pub max_size: usize,
    pub current_size: usize,
    pub oversize_skips: u64,
}

/// Get current monotonic time in nanoseconds.
fn current_time_nanos() -> u64 {
    #[cfg(target_os = "macos")]
    {
        use std::time::Instant;
        // Use a leaked base instant for consistent nanos
        static BASE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        let base = BASE.get_or_init(Instant::now);
        base.elapsed().as_nanos() as u64
    }

    #[cfg(target_os = "linux")]
    {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        unsafe {
            libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts);
        }
        (ts.tv_sec as u64) * 1_000_000_000 + (ts.tv_nsec as u64)
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        use std::time::Instant;
        static BASE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        let base = BASE.get_or_init(Instant::now);
        base.elapsed().as_nanos() as u64
    }
}

// ShmCache is Send+Sync because all mutations go through the shm rwlock
unsafe impl Send for ShmCache {}
unsafe impl Sync for ShmCache {}
