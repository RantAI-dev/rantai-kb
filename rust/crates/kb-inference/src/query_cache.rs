//! Least-recently-used cache for query embeddings.
//!
//! Matches `src/lib/rag/lru-cache.ts`: a read moves the entry to most-recent,
//! so the least-recently-used entry is the one evicted.

use std::collections::HashMap;

/// Cached query embeddings. About 8 MB at 4096-d Float32 with 256 entries.
#[derive(Default)]
pub struct QueryCache {
    map: HashMap<String, Vec<f32>>,
    /// Keys from least-recent (front) to most-recent (back).
    order: Vec<String>,
    capacity: usize,
}

impl QueryCache {
    #[must_use]
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            map: HashMap::new(),
            order: Vec::new(),
            capacity: cap,
        }
    }

    fn touch(&mut self, k: &str) {
        if let Some(pos) = self.order.iter().position(|x| x == k) {
            let key = self.order.remove(pos);
            self.order.push(key);
        }
    }

    /// Returns a clone of the value and marks the key most-recent.
    pub fn get(&mut self, k: &str) -> Option<Vec<f32>> {
        let value = self.map.get(k).cloned()?;
        self.touch(k);
        Some(value)
    }

    /// Inserts or replaces a value, evicting the least-recent entry when full.
    pub fn put(&mut self, k: String, v: Vec<f32>) {
        if self.map.contains_key(&k) {
            self.touch(&k);
            self.map.insert(k, v);
            return;
        }
        if self.capacity == 0 {
            return;
        }
        if self.order.len() >= self.capacity {
            let evicted = self.order.remove(0);
            self.map.remove(&evicted);
        }
        self.order.push(k.clone());
        self.map.insert(k, v);
    }
}

#[cfg(test)]
mod tests {
    use super::QueryCache;

    #[test]
    fn evicts_least_recently_inserted_when_untouched() {
        let mut c = QueryCache::with_capacity(2);
        c.put("a".into(), vec![1.0]);
        c.put("b".into(), vec![2.0]);
        c.put("c".into(), vec![3.0]);
        assert!(c.get("a").is_none());
        assert!(c.get("b").is_some());
        assert!(c.get("c").is_some());
    }

    #[test]
    fn read_refreshes_recency_like_typescript_lru() {
        let mut c = QueryCache::with_capacity(2);
        c.put("a".into(), vec![1.0]);
        c.put("b".into(), vec![2.0]);
        // Reading "a" makes "b" the least-recent entry.
        assert!(c.get("a").is_some());
        c.put("c".into(), vec![3.0]);
        assert!(c.get("b").is_none(), "b should have been evicted");
        assert!(c.get("a").is_some());
        assert!(c.get("c").is_some());
    }
}
