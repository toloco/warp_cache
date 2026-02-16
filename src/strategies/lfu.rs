use std::collections::{BTreeSet, HashMap};
use std::time::Instant;

use crate::entry::CacheEntry;
use crate::key::CacheKey;
use crate::strategies::EvictionStrategy;

/// Ordering key for the frequency index.
/// Lower frequency evicted first; ties broken by oldest creation time, then unique id.
#[derive(Clone)]
struct FreqKey {
    frequency: u64,
    created_at_nanos: u128,
    unique_id: u64,
    cache_key: CacheKey,
}

impl PartialEq for FreqKey {
    fn eq(&self, other: &Self) -> bool {
        self.frequency == other.frequency
            && self.created_at_nanos == other.created_at_nanos
            && self.unique_id == other.unique_id
    }
}

impl Eq for FreqKey {}

impl PartialOrd for FreqKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for FreqKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.frequency
            .cmp(&other.frequency)
            .then_with(|| self.created_at_nanos.cmp(&other.created_at_nanos))
            .then_with(|| self.unique_id.cmp(&other.unique_id))
    }
}

pub struct LfuStrategy {
    map: HashMap<CacheKey, (CacheEntry, FreqKey)>,
    index: BTreeSet<FreqKey>,
    epoch: Instant,
    capacity: usize,
    next_id: u64,
}

impl LfuStrategy {
    pub fn new(capacity: usize) -> Self {
        Self {
            map: HashMap::new(),
            index: BTreeSet::new(),
            epoch: Instant::now(),
            capacity,
            next_id: 0,
        }
    }

    fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
}

impl EvictionStrategy for LfuStrategy {
    fn insert(&mut self, key: CacheKey, entry: CacheEntry) {
        if let Some((_, old_fk)) = self.map.remove(&key) {
            self.index.remove(&old_fk);
        } else if self.map.len() >= self.capacity {
            if let Some(victim_fk) = self.index.iter().next().cloned() {
                self.index.remove(&victim_fk);
                self.map.remove(&victim_fk.cache_key);
            }
        }
        let id = self.alloc_id();
        let fk = FreqKey {
            frequency: entry.frequency,
            created_at_nanos: entry.created_at.duration_since(self.epoch).as_nanos(),
            unique_id: id,
            cache_key: key.clone(),
        };
        self.index.insert(fk.clone());
        self.map.insert(key, (entry, fk));
    }

    fn peek(&self, key: &CacheKey) -> Option<&CacheEntry> {
        self.map.get(key).map(|(entry, _)| entry)
    }

    fn record_access(&mut self, key: &CacheKey) {
        if !self.map.contains_key(key) {
            return;
        }

        // Remove old index entry
        let (_, old_fk) = &self.map[key];
        let old_fk = old_fk.clone();
        self.index.remove(&old_fk);

        let id = self.alloc_id();

        // Bump frequency, build new FreqKey
        let (entry, stored_fk) = self.map.get_mut(key).unwrap();
        entry.frequency += 1;

        let new_fk = FreqKey {
            frequency: entry.frequency,
            created_at_nanos: entry.created_at.duration_since(self.epoch).as_nanos(),
            unique_id: id,
            cache_key: key.clone(),
        };
        self.index.insert(new_fk.clone());
        *stored_fk = new_fk;
    }

    fn remove(&mut self, key: &CacheKey) -> Option<CacheEntry> {
        if let Some((entry, fk)) = self.map.remove(key) {
            self.index.remove(&fk);
            Some(entry)
        } else {
            None
        }
    }

    fn len(&self) -> usize {
        self.map.len()
    }

    fn clear(&mut self) {
        self.map.clear();
        self.index.clear();
    }

    fn capacity(&self) -> usize {
        self.capacity
    }
}
