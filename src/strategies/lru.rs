use hashlink::LruCache;

use crate::entry::CacheEntry;
use crate::key::CacheKey;
use crate::strategies::EvictionStrategy;

pub struct LruStrategy {
    cache: LruCache<CacheKey, CacheEntry>,
    cap: usize,
}

impl LruStrategy {
    pub fn new(capacity: usize) -> Self {
        Self {
            cache: LruCache::new(capacity),
            cap: capacity,
        }
    }
}

impl EvictionStrategy for LruStrategy {
    fn insert(&mut self, key: CacheKey, entry: CacheEntry) {
        // LruCache handles eviction automatically when at capacity
        self.cache.insert(key, entry);
    }

    fn get_mut(&mut self, key: &CacheKey) -> Option<&mut CacheEntry> {
        self.cache.get_mut(key)
    }

    fn remove(&mut self, key: &CacheKey) -> Option<CacheEntry> {
        self.cache.remove(key)
    }

    fn len(&self) -> usize {
        self.cache.len()
    }

    fn clear(&mut self) {
        self.cache.clear();
    }

    fn capacity(&self) -> usize {
        self.cap
    }
}
