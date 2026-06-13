//! Local chunk storage.

use crate::SwarmResult;
use nectar_primitives::{AnyChunk, ChunkAddress};
use vertex_swarm_primitives::StampedChunk;

/// Configuration for local store.
#[auto_impl::auto_impl(&, Arc)]
pub trait SwarmLocalStoreConfig {
    /// Cache capacity in number of chunks.
    fn cache_chunks(&self) -> u64;
}

/// Local chunk storage for Storer nodes.
#[auto_impl::auto_impl(&, Arc, Box)]
pub trait SwarmLocalStore: Send + Sync {
    /// Store a chunk locally.
    fn store(&self, chunk: &AnyChunk) -> SwarmResult<()>;

    /// Store a chunk together with the postage stamp it was accepted under.
    ///
    /// A serving store keeps the stamp so it can later answer retrievals (see
    /// [`Self::retrieve_stamped`]). The default drops the stamp and stores only
    /// the chunk, which keeps non-serving backends simple; the storer's
    /// `LocalStoreImpl` overrides this to persist the pairing.
    fn store_stamped(&self, chunk: &StampedChunk) -> SwarmResult<()> {
        self.store(chunk.chunk())
    }

    /// Retrieve a chunk from local storage.
    fn retrieve(&self, address: &ChunkAddress) -> SwarmResult<Option<AnyChunk>>;

    /// Retrieve a chunk together with the postage stamp it was stored under.
    ///
    /// Serving a retrieval over the wire requires the stamp that authorized the
    /// chunk, so a serving store pairs the chunk bytes with the stamp it took
    /// custody of on the push. The default returns `None`: a backend that does
    /// not persist stamps cannot answer retrieval requests, and the serve path
    /// treats that as a miss. Backends that take custody (see the storer's
    /// `LocalStoreImpl`) override this to return the stored pairing.
    fn retrieve_stamped(&self, _address: &ChunkAddress) -> SwarmResult<Option<StampedChunk>> {
        Ok(None)
    }

    /// Check if a chunk exists locally.
    fn has(&self, address: &ChunkAddress) -> bool;

    /// Remove a chunk from local storage.
    fn remove(&self, address: &ChunkAddress) -> SwarmResult<()>;
}
