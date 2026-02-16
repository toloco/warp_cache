pub mod fifo;
pub mod lfu;
pub mod lru;
pub mod mru;

use crate::entry::CacheEntry;
use crate::key::CacheKey;

pub trait EvictionStrategy: Send + Sync {
    fn insert(&mut self, key: CacheKey, entry: CacheEntry);
    fn peek(&self, key: &CacheKey) -> Option<&CacheEntry>;
    fn record_access(&mut self, key: &CacheKey);
    fn remove(&mut self, key: &CacheKey) -> Option<CacheEntry>;
    fn len(&self) -> usize;
    fn clear(&mut self);
    fn capacity(&self) -> usize;
}

/// Concrete enum wrapping all strategies — enables devirtualization + inlining.
pub enum StrategyEnum {
    Lru(lru::LruStrategy),
    Mru(mru::MruStrategy),
    Fifo(fifo::FifoStrategy),
    Lfu(lfu::LfuStrategy),
}

impl StrategyEnum {
    #[inline(always)]
    pub fn insert(&mut self, key: CacheKey, entry: CacheEntry) {
        match self {
            Self::Lru(s) => s.insert(key, entry),
            Self::Mru(s) => s.insert(key, entry),
            Self::Fifo(s) => s.insert(key, entry),
            Self::Lfu(s) => s.insert(key, entry),
        }
    }

    #[inline(always)]
    pub fn peek(&self, key: &CacheKey) -> Option<&CacheEntry> {
        match self {
            Self::Lru(s) => s.peek(key),
            Self::Mru(s) => s.peek(key),
            Self::Fifo(s) => s.peek(key),
            Self::Lfu(s) => s.peek(key),
        }
    }

    #[inline(always)]
    pub fn record_access(&mut self, key: &CacheKey) {
        match self {
            Self::Lru(s) => s.record_access(key),
            Self::Mru(s) => s.record_access(key),
            Self::Fifo(s) => s.record_access(key),
            Self::Lfu(s) => s.record_access(key),
        }
    }

    #[inline(always)]
    pub fn remove(&mut self, key: &CacheKey) -> Option<CacheEntry> {
        match self {
            Self::Lru(s) => s.remove(key),
            Self::Mru(s) => s.remove(key),
            Self::Fifo(s) => s.remove(key),
            Self::Lfu(s) => s.remove(key),
        }
    }

    #[inline(always)]
    pub fn len(&self) -> usize {
        match self {
            Self::Lru(s) => s.len(),
            Self::Mru(s) => s.len(),
            Self::Fifo(s) => s.len(),
            Self::Lfu(s) => s.len(),
        }
    }

    #[inline(always)]
    pub fn clear(&mut self) {
        match self {
            Self::Lru(s) => s.clear(),
            Self::Mru(s) => s.clear(),
            Self::Fifo(s) => s.clear(),
            Self::Lfu(s) => s.clear(),
        }
    }

    #[inline(always)]
    pub fn capacity(&self) -> usize {
        match self {
            Self::Lru(s) => s.capacity(),
            Self::Mru(s) => s.capacity(),
            Self::Fifo(s) => s.capacity(),
            Self::Lfu(s) => s.capacity(),
        }
    }
}
