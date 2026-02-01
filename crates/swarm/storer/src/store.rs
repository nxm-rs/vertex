//! LocalStore implementation.
//!
//! This module provides [`LocalStoreImpl`] which implements the
//! [`LocalStore`] trait from swarm-api.

use nectar_primitives::{AnyChunk, Chunk, ChunkAddress};
use tracing::{debug, trace};
use vertex_swarm_api::{SwarmError, SwarmLocalStore, SwarmResult};

use crate::{ChunkCache, ChunkStore, Reserve, StorerError};

/// LocalStore implementation backed by ChunkStore.
///
/// Provides chunk storage with caching and capacity management.
///
/// # Serialization Format
///
/// Chunks are stored as:
/// - Byte 0: chunk type (0 = content, 1 = SOC)
/// - Bytes 1..N: chunk data
/// - Bytes N..: stamp (if present)
///
/// Note: Currently stores a simplified format. Full reconstruction
/// requires the original chunk types from nectar-primitives.
pub struct LocalStoreImpl<S: ChunkStore> {
    /// The underlying chunk store.
    store: S,
    /// In-memory cache for hot chunks.
    cache: ChunkCache,
    /// Reserve capacity tracker.
    reserve: Reserve,
}

impl<S: ChunkStore> LocalStoreImpl<S> {
    /// Create a new local store.
    pub fn new(store: S, reserve: Reserve) -> Self {
        Self::with_cache(store, reserve, ChunkCache::new(1024))
    }

    /// Create with a custom cache.
    pub fn with_cache(store: S, reserve: Reserve, cache: ChunkCache) -> Self {
        Self {
            store,
            cache,
            reserve,
        }
    }

    /// Initialize the store from existing data.
    pub fn initialize(&self) -> Result<(), StorerError> {
        self.reserve.initialize_from(&self.store)
    }

    /// Get the reserve.
    pub fn reserve(&self) -> &Reserve {
        &self.reserve
    }

    /// Get cache statistics.
    pub fn cache_stats(&self) -> crate::cache::CacheStats {
        self.cache.stats()
    }

    /// Serialize a chunk to bytes.
    fn serialize_chunk(chunk: &AnyChunk) -> Vec<u8> {
        // Simple format: [type_byte][data]
        // Note: Stamps are handled separately in the postage system
        let mut bytes = Vec::new();
        match chunk {
            AnyChunk::Content(c) => {
                bytes.push(0);
                bytes.extend_from_slice(c.data());
            }
            AnyChunk::SingleOwner(c) => {
                bytes.push(1);
                bytes.extend_from_slice(c.data());
            }
            AnyChunk::Custom { data, .. } => {
                bytes.push(2);
                bytes.extend_from_slice(data);
            }
        }
        bytes
    }

    /// Deserialize bytes to a chunk.
    ///
    /// Note: This creates a simplified chunk. Full reconstruction would
    /// require more complex deserialization matching nectar-primitives format.
    fn deserialize_chunk(address: ChunkAddress, bytes: &[u8]) -> SwarmResult<AnyChunk> {
        use nectar_primitives::ContentChunk;

        if bytes.is_empty() {
            return Err(SwarmError::InvalidChunk {
                reason: "empty data".to_string(),
            });
        }

        let type_byte = bytes[0];
        let data = &bytes[1..];

        match type_byte {
            0..=2 => {
                // For now, treat all as content chunks
                // TODO: Proper deserialization when chunk format is finalized
                let chunk = ContentChunk::with_address(data.to_vec(), address).map_err(|e| {
                    SwarmError::InvalidChunk {
                        reason: e.to_string(),
                    }
                })?;
                Ok(AnyChunk::Content(chunk))
            }
            _ => Err(SwarmError::InvalidChunk {
                reason: format!("unknown chunk type: {}", type_byte),
            }),
        }
    }
}

impl<S: ChunkStore> SwarmLocalStore for LocalStoreImpl<S> {
    fn store(&self, chunk: &AnyChunk) -> SwarmResult<()> {
        let address = chunk.address();

        // Check if already stored
        if self.has(address) {
            trace!(%address, "Chunk already stored");
            return Ok(());
        }

        // Try to reserve space
        self.reserve
            .try_reserve(&self.store)
            .map_err(|e| SwarmError::Storage {
                message: e.to_string(),
            })?;

        // Serialize and store
        let bytes = Self::serialize_chunk(chunk);
        self.store
            .put(address, &bytes)
            .map_err(|e| SwarmError::Storage {
                message: e.to_string(),
            })?;

        // Update reserve and cache
        self.reserve.on_added();
        self.cache.put(*address, bytes);

        debug!(%address, "Stored chunk");
        Ok(())
    }

    fn retrieve(&self, address: &ChunkAddress) -> SwarmResult<Option<AnyChunk>> {
        // Check cache first
        if let Some(bytes) = self.cache.get(address) {
            trace!(%address, "Cache hit");
            return Ok(Some(Self::deserialize_chunk(*address, &bytes)?));
        }

        // Check store
        let bytes = self.store.get(address).map_err(|e| SwarmError::Storage {
            message: e.to_string(),
        })?;

        match bytes {
            Some(data) => {
                // Cache the result
                let chunk = Self::deserialize_chunk(*address, &data)?;
                self.cache.put(*address, data);
                Ok(Some(chunk))
            }
            None => Ok(None),
        }
    }

    fn has(&self, address: &ChunkAddress) -> bool {
        // Check cache first
        if self.cache.contains(address) {
            return true;
        }

        // Check store
        self.store.contains(address).unwrap_or(false)
    }

    fn remove(&self, address: &ChunkAddress) -> SwarmResult<()> {
        // Remove from cache
        self.cache.remove(address);

        // Remove from store
        self.store
            .delete(address)
            .map_err(|e| SwarmError::Storage {
                message: e.to_string(),
            })?;

        // Update reserve
        self.reserve.on_removed();

        debug!(%address, "Removed chunk");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::memory::MemoryChunkStore;
    use nectar_primitives::ContentChunk;

    fn test_chunk(n: u8) -> AnyChunk {
        let mut addr_bytes = [0u8; 32];
        addr_bytes[0] = n;
        let address = ChunkAddress::new(addr_bytes);
        let data = format!("chunk data {}", n).into_bytes();
        AnyChunk::Content(ContentChunk::with_address(data, address).unwrap())
    }

    #[test]
    fn test_store_retrieve() {
        let store = MemoryChunkStore::new();
        let reserve = Reserve::new(100);
        let local_store = LocalStoreImpl::new(store, reserve);

        let chunk = test_chunk(1);
        let address = chunk.address();

        // Store
        local_store.store(&chunk).unwrap();

        // Verify exists
        assert!(local_store.has(address));

        // Retrieve
        let retrieved = local_store.retrieve(address).unwrap();
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().address(), address);
    }

    #[test]
    fn test_remove() {
        let store = MemoryChunkStore::new();
        let reserve = Reserve::new(100);
        let local_store = LocalStoreImpl::new(store, reserve);

        let chunk = test_chunk(2);
        let address = chunk.address();

        local_store.store(&chunk).unwrap();
        assert!(local_store.has(address));

        local_store.remove(address).unwrap();
        assert!(!local_store.has(address));
    }

    #[test]
    fn test_idempotent_store() {
        let store = MemoryChunkStore::new();
        let reserve = Reserve::new(100);
        let local_store = LocalStoreImpl::new(store, reserve);

        let chunk = test_chunk(3);

        // Store twice
        local_store.store(&chunk).unwrap();
        local_store.store(&chunk).unwrap();

        // Should only count once
        assert_eq!(local_store.reserve().count(), 1);
    }

    #[test]
    fn test_capacity_limit() {
        let store = MemoryChunkStore::new();
        let reserve = Reserve::new(2);
        let local_store = LocalStoreImpl::new(store, reserve);

        local_store.store(&test_chunk(1)).unwrap();
        local_store.store(&test_chunk(2)).unwrap();

        // Third should fail
        let result = local_store.store(&test_chunk(3));
        assert!(result.is_err());
    }
}
