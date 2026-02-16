use hashlink::LinkedHashMap;

use crate::entry::CacheEntry;
use crate::key::CacheKey;
use crate::strategies::EvictionStrategy;

/// MRU: evicts the most recently used entry.
/// On access, the entry moves to the back. On eviction, remove the back.
pub struct MruStrategy {
    map: LinkedHashMap<CacheKey, CacheEntry>,
    capacity: usize,
}

impl MruStrategy {
    pub fn new(capacity: usize) -> Self {
        Self {
            map: LinkedHashMap::new(),
            capacity,
        }
    }
}

impl EvictionStrategy for MruStrategy {
    fn insert(&mut self, key: CacheKey, entry: CacheEntry) {
        if self.map.contains_key(&key) {
            self.map.remove(&key);
        } else if self.map.len() >= self.capacity {
            // Evict most recently used (back)
            self.map.pop_back();
        }
        // Insert at back (most recent position)
        self.map.insert(key, entry);
    }

    fn peek(&self, key: &CacheKey) -> Option<&CacheEntry> {
        self.map.get(key)
    }

    fn record_access(&mut self, key: &CacheKey) {
        // Move to back (most recent) by removing and re-inserting
        if let Some(entry) = self.map.remove(key) {
            let key_clone = key.clone();
            self.map.insert(key_clone, entry);
        }
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
