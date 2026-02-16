use hashlink::LinkedHashMap;

use crate::entry::CacheEntry;
use crate::key::CacheKey;
use crate::strategies::EvictionStrategy;

pub struct FifoStrategy {
    map: LinkedHashMap<CacheKey, CacheEntry>,
    capacity: usize,
}

impl FifoStrategy {
    pub fn new(capacity: usize) -> Self {
        Self {
            map: LinkedHashMap::new(),
            capacity,
        }
    }
}

impl EvictionStrategy for FifoStrategy {
    fn insert(&mut self, key: CacheKey, entry: CacheEntry) {
        if self.map.contains_key(&key) {
            // Replace existing without changing order
            self.map.replace(key, entry);
            return;
        }
        if self.map.len() >= self.capacity {
            // Evict oldest (front)
            self.map.pop_front();
        }
        self.map.insert(key, entry);
    }

    fn peek(&self, key: &CacheKey) -> Option<&CacheEntry> {
        self.map.get(key)
    }

    fn record_access(&mut self, _key: &CacheKey) {
        // FIFO: no reordering on access
    }

    fn remove(&mut self, key: &CacheKey) -> Option<CacheEntry> {
        self.map.remove(key)
    }

    fn len(&self) -> usize {
        self.map.len()
    }

    fn clear(&mut self) {
        self.map.clear();
    }

    fn capacity(&self) -> usize {
        self.capacity
    }
}
