/// Shared-memory cache backend.
///
/// Provides `ShmCache` — a cross-process LRU/MRU/FIFO/LFU cache backed
/// by mmap. All data (header, hash table, slab arena) lives in a single
/// memory-mapped file. A separate mmap file holds the seqlock.
///
/// Read path uses an optimistic seqlock: lock-free hash lookup + value copy,
/// then a brief write lock only when ordering updates are needed (LRU/MRU/LFU).
/// FIFO reads are fully lock-free. Stats are updated via atomics (no lock).
pub mod hashtable;
pub mod layout;
pub mod lock;
pub mod ordering;
pub mod region;

use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

use layout::{Bucket, Header, SlotHeader, BUCKET_EMPTY, SLOT_HEADER_SIZE, SLOT_NONE};
use lock::ShmSeqLock;
use region::ShmRegion;

/// Result of a cache get operation.
pub enum ShmGetResult {
    Hit(Vec<u8>),
    Miss,
}

/// Result of the optimistic (lock-free) read phase.
enum OptimisticResult {
    /// Cache hit — value bytes copied, slot_index for ordering update.
    Hit { value: Vec<u8>, slot_index: i32 },
    /// Key not found.
    Miss,
    /// Entry found but TTL expired — slot_index for cleanup.
    Expired { slot_index: i32 },
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

    fn lock(&self) -> ShmSeqLock {
        self.region.lock()
    }

    fn header(&self) -> &Header {
        self.region.header()
    }

    /// Get the mutable header pointer. Caller must hold write lock.
    #[allow(clippy::mut_from_ref)]
    unsafe fn header_mut(&self) -> &mut Header {
        &mut *(self.region.base_ptr() as *mut Header)
    }

    fn base_ptr(&self) -> *const u8 {
        self.region.base_ptr()
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

    // --- Atomic stat accessors (no lock needed) ---

    /// Atomic reference to the `hits` field in the header.
    #[inline]
    fn atomic_hits(&self) -> &AtomicU64 {
        // Header offset of `hits` = 16 (after magic[8] + ttl_nanos[8])
        unsafe { &*(self.base_ptr().add(16) as *const AtomicU64) }
    }

    /// Atomic reference to the `misses` field in the header.
    #[inline]
    fn atomic_misses(&self) -> &AtomicU64 {
        // Header offset of `misses` = 24
        unsafe { &*(self.base_ptr().add(24) as *const AtomicU64) }
    }

    /// Atomic reference to the `oversize_skips` field in the header.
    #[inline]
    fn atomic_oversize_skips(&self) -> &AtomicU64 {
        // Header offset of `oversize_skips` = 32
        unsafe { &*(self.base_ptr().add(32) as *const AtomicU64) }
    }

    /// Check if key/value sizes exceed limits. Returns true if oversize.
    pub fn is_oversize(&self, key_bytes: &[u8], value_bytes: &[u8]) -> bool {
        let h = self.header();
        key_bytes.len() > h.max_key_size as usize || value_bytes.len() > h.max_value_size as usize
    }

    /// Bounds-checked hash table lookup for the optimistic read path.
    ///
    /// Mirrors `hashtable::ht_lookup` but adds bounds checks to guard against
    /// torn reads during a concurrent write (the seqlock will detect the tear,
    /// but we must not segfault before we get to `read_validate`).
    ///
    /// Returns `Some((slot_index, value_bytes))` on hit, `None` on miss.
    #[inline]
    #[allow(clippy::too_many_arguments)]
    unsafe fn ht_lookup_checked(
        &self,
        ht_base: *const u8,
        ht_capacity: u32,
        slab_base: *const u8,
        slot_size: u32,
        capacity: u32,
        max_data_size: usize,
        key_hash: u64,
        key_bytes: &[u8],
        ttl_nanos: u64,
    ) -> OptimisticResult {
        let mask = ht_capacity.wrapping_sub(1);
        let mut idx = (key_hash as u32) & mask;

        for _ in 0..ht_capacity {
            let bucket = &*(ht_base.add(idx as usize * Bucket::SIZE) as *const Bucket);

            if bucket.slot_index == BUCKET_EMPTY {
                return OptimisticResult::Miss;
            }

            if bucket.hash == key_hash {
                let slot_index = bucket.slot_index;

                // Bounds check: slot_index must be in [0, capacity)
                if slot_index < 0 || slot_index as u32 >= capacity {
                    return OptimisticResult::Miss; // torn read, will be caught by seqlock
                }

                let slot_ptr = slab_base.add(slot_index as usize * slot_size as usize);
                let slot = &*(slot_ptr as *const SlotHeader);

                if slot.occupied != 0 && slot.key_len == key_bytes.len() as u32 {
                    let key_len = slot.key_len as usize;
                    let value_len = slot.value_len as usize;

                    // Bounds check: key + value must fit in slot data area
                    if key_len + value_len > max_data_size {
                        return OptimisticResult::Miss; // torn read
                    }

                    let stored_key =
                        std::slice::from_raw_parts(slot_ptr.add(SLOT_HEADER_SIZE), key_len);
                    if stored_key == key_bytes {
                        // Check TTL
                        if ttl_nanos > 0 {
                            let now = current_time_nanos();
                            if now.saturating_sub(slot.created_at_nanos) > ttl_nanos {
                                return OptimisticResult::Expired { slot_index };
                            }
                        }

                        // Copy value bytes
                        let value_ptr = slot_ptr.add(SLOT_HEADER_SIZE + key_len);
                        let value = std::slice::from_raw_parts(value_ptr, value_len).to_vec();
                        return OptimisticResult::Hit { value, slot_index };
                    }
                }
            }

            idx = (idx + 1) & mask;
        }

        OptimisticResult::Miss
    }

    /// Optimistic lock-free read using the seqlock.
    /// Retries if a writer was active during the read.
    unsafe fn get_optimistic(
        &self,
        lock: &ShmSeqLock,
        key_hash: u64,
        key_bytes: &[u8],
    ) -> OptimisticResult {
        loop {
            let seq = lock.read_begin();

            // Read header fields we need (may be torn — that's OK, seqlock catches it)
            let h = self.header();
            let ht_capacity = h.ht_capacity;
            let slot_size = h.slot_size;
            let capacity = h.capacity;
            let ttl_nanos = h.ttl_nanos;
            let max_data_size = (h.max_key_size + h.max_value_size) as usize;

            let result = self.ht_lookup_checked(
                self.ht_base(),
                ht_capacity,
                self.slab_base(),
                slot_size,
                capacity,
                max_data_size,
                key_hash,
                key_bytes,
                ttl_nanos,
            );

            if lock.read_validate(seq) {
                return result;
            }
            // Writer was active — retry
        }
    }

    /// Look up a key (by hash + serialized bytes). Returns a copy of the value bytes on hit.
    ///
    /// Uses optimistic seqlock reads. Only acquires the write lock when ordering
    /// needs updating (LRU/MRU/LFU hit) or when removing an expired entry.
    pub fn get(&self, key_hash: u64, key_bytes: &[u8]) -> ShmGetResult {
        let lock = self.lock();

        let result = unsafe { self.get_optimistic(&lock, key_hash, key_bytes) };

        match result {
            OptimisticResult::Hit { value, slot_index } => {
                let strategy = self.header().strategy;

                // FIFO: no ordering update needed — fully lock-free
                if strategy != 2 {
                    // LRU/MRU/LFU: brief write lock for ordering update
                    lock.write_lock();
                    unsafe {
                        // Re-verify the slot is still valid (another writer may have evicted it)
                        let slot_size = self.header().slot_size;
                        let slot_ptr = self
                            .slab_base()
                            .add(slot_index as usize * slot_size as usize);
                        let slot = &*(slot_ptr as *const SlotHeader);
                        if slot.occupied != 0 && slot.key_hash == key_hash {
                            let header = self.header_mut();
                            ordering::on_access(
                                header,
                                self.slab_base_mut(),
                                slot_size,
                                slot_index,
                                strategy,
                            );
                        }
                    }
                    lock.write_unlock();
                }

                // Stats: atomic, no lock needed
                self.atomic_hits().fetch_add(1, AtomicOrdering::Relaxed);
                ShmGetResult::Hit(value)
            }
            OptimisticResult::Miss => {
                self.atomic_misses().fetch_add(1, AtomicOrdering::Relaxed);
                ShmGetResult::Miss
            }
            OptimisticResult::Expired { slot_index } => {
                // Need write lock to remove the expired entry
                lock.write_lock();
                unsafe {
                    // Re-verify the slot is still the same expired entry
                    let slot_size = self.header().slot_size;
                    let slot_ptr = self
                        .slab_base()
                        .add(slot_index as usize * slot_size as usize);
                    let slot = &*(slot_ptr as *const SlotHeader);
                    if slot.occupied != 0 && slot.key_hash == key_hash {
                        // Re-read key bytes to pass to remove_slot
                        let key_len = slot.key_len as usize;
                        let stored_key =
                            std::slice::from_raw_parts(slot_ptr.add(SLOT_HEADER_SIZE), key_len);
                        // Only remove if key actually matches (slot could have been reused)
                        if stored_key == key_bytes {
                            self.remove_slot(slot_index, key_bytes);
                        }
                    }
                }
                lock.write_unlock();

                self.atomic_misses().fetch_add(1, AtomicOrdering::Relaxed);
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

    /// Increment oversize skip counter. Lock-free via atomic.
    pub fn record_oversize_skip(&self) {
        self.atomic_oversize_skips()
            .fetch_add(1, AtomicOrdering::Relaxed);
    }

    /// Get cache statistics. Lock-free via atomic loads.
    pub fn info(&self) -> ShmCacheInfo {
        let h = self.header();
        ShmCacheInfo {
            hits: self.atomic_hits().load(AtomicOrdering::Relaxed),
            misses: self.atomic_misses().load(AtomicOrdering::Relaxed),
            max_size: h.capacity as usize,
            current_size: h.current_size as usize,
            oversize_skips: self.atomic_oversize_skips().load(AtomicOrdering::Relaxed),
        }
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

// ShmCache is Send+Sync because all mutations go through the shm seqlock
unsafe impl Send for ShmCache {}
unsafe impl Sync for ShmCache {}
