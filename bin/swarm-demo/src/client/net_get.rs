//! Network-backed async chunk getter for the file joiner and manifest walk:
//! seeded local cache first, then the network.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use nectar_primitives::store::{ChunkGet, ChunkStoreError};
use nectar_primitives::{AnyChunk, ChunkAddress, DEFAULT_BODY_SIZE};
use vertex_swarm_api::SwarmChunkProvider;

/// A `Send + Sync` chunk getter: local cache first, then the network.
#[derive(Clone)]
pub struct NetworkChunkGet {
    local: Arc<Mutex<HashMap<ChunkAddress, AnyChunk>>>,
    provider: Arc<dyn SwarmChunkProvider>,
}

impl NetworkChunkGet {
    /// Build a getter over `provider`, seeding the local cache with `seed` chunks.
    pub fn new(
        provider: Arc<dyn SwarmChunkProvider>,
        seed: HashMap<ChunkAddress, AnyChunk>,
    ) -> Self {
        Self {
            local: Arc::new(Mutex::new(seed)),
            provider,
        }
    }
}

impl ChunkGet<DEFAULT_BODY_SIZE> for NetworkChunkGet {
    type Error = ChunkStoreError;

    async fn get(&self, address: &ChunkAddress) -> Result<AnyChunk, Self::Error> {
        if let Some(chunk) = self
            .local
            .lock()
            .expect("cache mutex")
            .get(address)
            .cloned()
        {
            return Ok(chunk);
        }
        let result = self
            .provider
            .retrieve_chunk(address)
            .await
            .map_err(|e| ChunkStoreError::Other(e.to_string().into()))?;
        // Cache the fetched chunk so a re-read (e.g. the joiner revisiting an
        // intermediate node) does not hit the network twice.
        self.local
            .lock()
            .expect("cache mutex")
            .insert(*result.chunk.address(), result.chunk.clone());
        Ok(result.chunk)
    }
}
