//! LRU chunk cache for hot data.
//!
//! The [`ChunkCache`] provides a fast in-memory cache for frequently
//! accessed chunks, reducing disk reads.

use lru::LruCache;
use parking_lot::Mutex;
use std::num::NonZeroUsize;
use vertex_primitives::ChunkAddress;

/// LRU cache for chunk data.
///
/// Caches recently accessed chunks to reduce disk I/O.
pub struct ChunkCache {
    cache: Mutex<LruCache<ChunkAddress, Vec<u8>>>,
    hits: std::sync::atomic::AtomicU64,
    misses: std::sync::atomic::AtomicU64,
}

impl ChunkCache {
    /// Create a new cache with the given capacity.
    pub fn new(capacity: usize) -> Self {
        let cap = NonZeroUsize::new(capacity).unwrap_or(NonZeroUsize::new(1).unwrap());
        Self {
            cache: Mutex::new(LruCache::new(cap)),
            hits: std::sync::atomic::AtomicU64::new(0),
            misses: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Get a chunk from the cache.
    pub fn get(&self, address: &ChunkAddress) -> Option<Vec<u8>> {
        let mut cache = self.cache.lock();
        if let Some(data) = cache.get(address) {
            self.hits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Some(data.clone())
        } else {
            self.misses
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            None
        }
    }

    /// Put a chunk into the cache.
    pub fn put(&self, address: ChunkAddress, data: Vec<u8>) {
        let mut cache = self.cache.lock();
        cache.put(address, data);
    }

    /// Remove a chunk from the cache.
    pub fn remove(&self, address: &ChunkAddress) {
        let mut cache = self.cache.lock();
        cache.pop(address);
    }

    /// Check if a chunk is in the cache.
    pub fn contains(&self, address: &ChunkAddress) -> bool {
        let cache = self.cache.lock();
        cache.contains(address)
    }

    /// Get the number of cached chunks.
    pub fn len(&self) -> usize {
        let cache = self.cache.lock();
        cache.len()
    }

    /// Check if the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Clear the cache.
    pub fn clear(&self) {
        let mut cache = self.cache.lock();
        cache.clear();
    }

    /// Get cache statistics.
    pub fn stats(&self) -> CacheStats {
        let hits = self.hits.load(std::sync::atomic::Ordering::Relaxed);
        let misses = self.misses.load(std::sync::atomic::Ordering::Relaxed);
        let cache = self.cache.lock();

        CacheStats {
            capacity: cache.cap().get(),
            size: cache.len(),
            hits,
            misses,
            hit_rate: if hits + misses > 0 {
                (hits as f64 / (hits + misses) as f64) * 100.0
            } else {
                0.0
            },
        }
    }
}

/// Cache statistics.
#[derive(Debug, Clone)]
pub struct CacheStats {
    /// Maximum cache capacity.
    pub capacity: usize,
    /// Current cache size.
    pub size: usize,
    /// Cache hits.
    pub hits: u64,
    /// Cache misses.
    pub misses: u64,
    /// Hit rate percentage.
    pub hit_rate: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_address(n: u8) -> ChunkAddress {
        let mut bytes = [0u8; 32];
        bytes[0] = n;
        ChunkAddress::new(bytes)
    }

    #[test]
    fn test_cache_put_get() {
        let cache = ChunkCache::new(10);

        let addr = test_address(1);
        let data = b"hello world".to_vec();

        cache.put(addr, data.clone());
        let retrieved = cache.get(&addr);

        assert_eq!(retrieved, Some(data));
    }

    #[test]
    fn test_cache_miss() {
        let cache = ChunkCache::new(10);

        let addr = test_address(1);
        let retrieved = cache.get(&addr);

        assert!(retrieved.is_none());
    }

    #[test]
    fn test_cache_eviction() {
        let cache = ChunkCache::new(2);

        cache.put(test_address(1), b"one".to_vec());
        cache.put(test_address(2), b"two".to_vec());

        // This should evict address 1
        cache.put(test_address(3), b"three".to_vec());

        assert!(cache.get(&test_address(1)).is_none());
        assert!(cache.get(&test_address(2)).is_some());
        assert!(cache.get(&test_address(3)).is_some());
    }

    #[test]
    fn test_cache_stats() {
        let cache = ChunkCache::new(10);

        cache.put(test_address(1), b"data".to_vec());

        cache.get(&test_address(1)); // hit
        cache.get(&test_address(2)); // miss

        let stats = cache.stats();
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.misses, 1);
        assert_eq!(stats.hit_rate, 50.0);
    }
}
