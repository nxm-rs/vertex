//! Chunk storage backend trait.
//!
//! The [`ChunkStore`] trait abstracts over different storage backends,
//! allowing implementations like redb, sled, or in-memory for testing.

use crate::StorerResult;
use nectar_primitives::ChunkAddress;

/// Chunk storage backend trait.
///
/// This is the low-level interface for chunk persistence.
/// Different backends (redb, memory, etc.) implement this trait.
///
/// # Chunk Data Format
///
/// Chunks are stored as raw bytes. The format is:
/// - First byte: chunk type (0 = content, 1 = SOC, etc.)
/// - Remaining bytes: chunk data + stamp
///
/// # Thread Safety
///
/// Implementations must be thread-safe (Send + Sync).
pub trait ChunkStore: Send + Sync {
    /// Store a chunk's raw data.
    ///
    /// If the chunk already exists, this is a no-op.
    fn put(&self, address: &ChunkAddress, data: &[u8]) -> StorerResult<()>;

    /// Get a chunk's raw data.
    ///
    /// Returns `None` if the chunk doesn't exist.
    fn get(&self, address: &ChunkAddress) -> StorerResult<Option<Vec<u8>>>;

    /// Check if a chunk exists.
    fn contains(&self, address: &ChunkAddress) -> StorerResult<bool>;

    /// Remove a chunk.
    ///
    /// Returns `Ok(())` even if the chunk didn't exist.
    fn delete(&self, address: &ChunkAddress) -> StorerResult<()>;

    /// Get the count of stored chunks.
    fn count(&self) -> StorerResult<u64>;

    /// Iterate over all chunk addresses.
    ///
    /// The callback receives each address. Return `false` to stop iteration.
    fn for_each<F>(&self, callback: F) -> StorerResult<()>
    where
        F: FnMut(&ChunkAddress) -> bool;
}

/// In-memory chunk store for testing.
#[cfg(test)]
pub(crate) mod memory {
    use super::*;
    use parking_lot::RwLock;
    use std::collections::HashMap;

    /// Simple in-memory chunk store.
    #[derive(Default)]
    pub(crate) struct MemoryChunkStore {
        chunks: RwLock<HashMap<ChunkAddress, Vec<u8>>>,
    }

    impl MemoryChunkStore {
        /// Create a new empty memory store.
        pub(crate) fn new() -> Self {
            Self::default()
        }
    }

    impl ChunkStore for MemoryChunkStore {
        fn put(&self, address: &ChunkAddress, data: &[u8]) -> StorerResult<()> {
            let mut chunks = self.chunks.write();
            chunks.entry(*address).or_insert_with(|| data.to_vec());
            Ok(())
        }

        fn get(&self, address: &ChunkAddress) -> StorerResult<Option<Vec<u8>>> {
            let chunks = self.chunks.read();
            Ok(chunks.get(address).cloned())
        }

        fn contains(&self, address: &ChunkAddress) -> StorerResult<bool> {
            let chunks = self.chunks.read();
            Ok(chunks.contains_key(address))
        }

        fn delete(&self, address: &ChunkAddress) -> StorerResult<()> {
            let mut chunks = self.chunks.write();
            chunks.remove(address);
            Ok(())
        }

        fn count(&self) -> StorerResult<u64> {
            let chunks = self.chunks.read();
            Ok(chunks.len() as u64)
        }

        fn for_each<F>(&self, mut callback: F) -> StorerResult<()>
        where
            F: FnMut(&ChunkAddress) -> bool,
        {
            let chunks = self.chunks.read();
            for address in chunks.keys() {
                if !callback(address) {
                    break;
                }
            }
            Ok(())
        }
    }
}
