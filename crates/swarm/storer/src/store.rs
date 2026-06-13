//! LocalStore implementation.
//!
//! This module provides [`LocalStoreImpl`] which implements the
//! [`LocalStore`] trait from swarm-api.

use nectar_primitives::{AnyChunk, Chunk, ChunkAddress};
use tracing::{debug, trace};
use vertex_swarm_api::{Stamp, StampedChunk, SwarmError, SwarmLocalStore, SwarmResult};

use crate::{ChunkCache, ChunkStore, Reserve, StorerError};

/// Length in bytes of a serialized postage stamp.
const STAMP_LEN: usize = 113;

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

    /// Serialize a chunk and an optional stamp to bytes.
    ///
    /// Layout: `[stamp_flag][type_byte][data][stamp?]`. The leading `stamp_flag`
    /// is `1` when a 113-byte stamp trailer follows the chunk data and `0` when
    /// no stamp was persisted. Persisting the stamp lets the store answer
    /// retrievals over the wire, which require the stamp that authorized the
    /// chunk.
    fn serialize_chunk(chunk: &AnyChunk, stamp: Option<&Stamp>) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.push(u8::from(stamp.is_some()));
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
        if let Some(stamp) = stamp {
            bytes.extend_from_slice(&stamp.to_bytes());
        }
        bytes
    }

    /// Split serialized bytes into the chunk payload and the optional stamp.
    ///
    /// Returns the `(type_byte, data, stamp)` triple. A malformed stamp trailer
    /// surfaces as an invalid-chunk error rather than being silently dropped.
    fn split_serialized(
        address: ChunkAddress,
        bytes: &[u8],
    ) -> SwarmResult<(u8, &[u8], Option<Stamp>)> {
        let (&stamp_flag, rest) = bytes.split_first().ok_or(SwarmError::InvalidChunk {
            address: Some(address),
            reason: "empty data".to_string(),
        })?;
        let (&type_byte, payload) = rest.split_first().ok_or(SwarmError::InvalidChunk {
            address: Some(address),
            reason: "missing chunk type".to_string(),
        })?;

        if stamp_flag == 0 {
            return Ok((type_byte, payload, None));
        }

        if payload.len() < STAMP_LEN {
            return Err(SwarmError::InvalidChunk {
                address: Some(address),
                reason: "stamp trailer truncated".to_string(),
            });
        }
        let (data, stamp_bytes) = payload.split_at(payload.len() - STAMP_LEN);
        let stamp = Stamp::try_from_slice(stamp_bytes).map_err(|e| SwarmError::InvalidChunk {
            address: Some(address),
            reason: e.to_string(),
        })?;
        Ok((type_byte, data, Some(stamp)))
    }

    /// Deserialize bytes to a chunk, ignoring any stamp trailer.
    ///
    /// Note: This creates a simplified chunk. Full reconstruction would
    /// require more complex deserialization matching nectar-primitives format.
    fn deserialize_chunk(address: ChunkAddress, bytes: &[u8]) -> SwarmResult<AnyChunk> {
        let (type_byte, data, _stamp) = Self::split_serialized(address, bytes)?;
        Self::chunk_from_payload(address, type_byte, data)
    }

    /// Deserialize bytes to a chunk and its stamp, when a stamp was persisted.
    fn deserialize_stamped(
        address: ChunkAddress,
        bytes: &[u8],
    ) -> SwarmResult<Option<StampedChunk>> {
        let (type_byte, data, stamp) = Self::split_serialized(address, bytes)?;
        let Some(stamp) = stamp else {
            return Ok(None);
        };
        let chunk = Self::chunk_from_payload(address, type_byte, data)?;
        Ok(Some(StampedChunk::new(chunk, stamp)))
    }

    /// Reconstruct a chunk from its payload bytes.
    fn chunk_from_payload(
        address: ChunkAddress,
        type_byte: u8,
        data: &[u8],
    ) -> SwarmResult<AnyChunk> {
        use nectar_primitives::ContentChunk;

        match type_byte {
            0..=2 => {
                // For now, treat all as content chunks
                // TODO: Proper deserialization when chunk format is finalized
                let chunk = ContentChunk::with_address(data.to_vec(), address).map_err(|e| {
                    SwarmError::InvalidChunk {
                        address: Some(address),
                        reason: e.to_string(),
                    }
                })?;
                Ok(AnyChunk::Content(chunk))
            }
            _ => Err(SwarmError::InvalidChunk {
                address: Some(address),
                reason: format!("unknown chunk type: {}", type_byte),
            }),
        }
    }
}

impl<S: ChunkStore> LocalStoreImpl<S> {
    /// Store serialized chunk bytes for `address`, updating the reserve and cache.
    fn put_serialized(&self, address: &ChunkAddress, bytes: Vec<u8>) -> SwarmResult<()> {
        // Check if already stored
        if self.has(address) {
            trace!(%address, "Chunk already stored");
            return Ok(());
        }

        // Try to reserve space
        self.reserve
            .try_reserve(&self.store)
            .map_err(SwarmError::storage)?;

        self.store
            .put(address, &bytes)
            .map_err(SwarmError::storage)?;

        // Update reserve and cache
        self.reserve.on_added();
        self.cache.put(*address, bytes);

        debug!(%address, "Stored chunk");
        Ok(())
    }
}

impl<S: ChunkStore> SwarmLocalStore for LocalStoreImpl<S> {
    fn store(&self, chunk: &AnyChunk) -> SwarmResult<()> {
        let address = chunk.address();
        let bytes = Self::serialize_chunk(chunk, None);
        self.put_serialized(address, bytes)
    }

    fn store_stamped(&self, chunk: &StampedChunk) -> SwarmResult<()> {
        let address = chunk.address();
        let bytes = Self::serialize_chunk(chunk.chunk(), Some(chunk.stamp()));
        self.put_serialized(address, bytes)
    }

    fn retrieve_stamped(&self, address: &ChunkAddress) -> SwarmResult<Option<StampedChunk>> {
        if let Some(bytes) = self.cache.get(address) {
            trace!(%address, "Cache hit (stamped)");
            return Self::deserialize_stamped(*address, &bytes);
        }

        match self.store.get(address).map_err(SwarmError::storage)? {
            Some(data) => {
                let stamped = Self::deserialize_stamped(*address, &data)?;
                self.cache.put(*address, data);
                Ok(stamped)
            }
            None => Ok(None),
        }
    }

    fn retrieve(&self, address: &ChunkAddress) -> SwarmResult<Option<AnyChunk>> {
        // Check cache first
        if let Some(bytes) = self.cache.get(address) {
            trace!(%address, "Cache hit");
            return Ok(Some(Self::deserialize_chunk(*address, &bytes)?));
        }

        // Check store
        let bytes = self.store.get(address).map_err(SwarmError::storage)?;

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
        self.store.delete(address).map_err(SwarmError::storage)?;

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

    fn test_stamp() -> Stamp {
        use alloy_primitives::{B256, Signature};
        let sig = Signature::from_raw(&[1u8; 65]).expect("valid signature");
        Stamp::new(B256::repeat_byte(0xaa), 3, 7, 42, sig)
    }

    #[test]
    fn stamped_store_round_trips_the_stamp() {
        let store = MemoryChunkStore::new();
        let reserve = Reserve::new(100);
        let local_store = LocalStoreImpl::new(store, reserve);

        let chunk = test_chunk(5);
        let address = *chunk.address();
        let stamp = test_stamp();
        let stamped = StampedChunk::new(chunk, stamp.clone());

        local_store.store_stamped(&stamped).unwrap();

        // The stamp-aware retrieve returns the chunk paired with the stored stamp.
        let got = local_store
            .retrieve_stamped(&address)
            .unwrap()
            .expect("chunk present");
        assert_eq!(*got.address(), address);
        assert_eq!(got.stamp().to_bytes(), stamp.to_bytes());

        // The plain retrieve still returns the chunk, ignoring the stamp trailer.
        let plain = local_store
            .retrieve(&address)
            .unwrap()
            .expect("chunk present");
        assert_eq!(*plain.address(), address);
    }

    #[test]
    fn unstamped_store_has_no_stamp_to_serve() {
        let store = MemoryChunkStore::new();
        let reserve = Reserve::new(100);
        let local_store = LocalStoreImpl::new(store, reserve);

        let chunk = test_chunk(6);
        let address = *chunk.address();
        local_store.store(&chunk).unwrap();

        // A chunk stored without a stamp cannot be served as a stamped chunk.
        assert!(local_store.retrieve_stamped(&address).unwrap().is_none());
        // But it is still retrievable as a plain chunk.
        assert!(local_store.retrieve(&address).unwrap().is_some());
    }
}
