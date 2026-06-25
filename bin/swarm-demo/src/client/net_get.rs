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

    /// The getter's shared chunk map. A concurrent prefetch inserts into this so
    /// the joiner's ordered reads find prefetched chunks without re-fetching.
    pub fn shared(&self) -> Arc<Mutex<HashMap<ChunkAddress, AnyChunk>>> {
        Arc::clone(&self.local)
    }

    /// The provider backing network fetches, shared with this getter.
    pub fn provider(&self) -> Arc<dyn SwarmChunkProvider> {
        Arc::clone(&self.provider)
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
            .map_err(|e| ChunkStoreError::Other(e.to_string()))?;
        // Cache the fetched chunk so a re-read (e.g. the joiner revisiting an
        // intermediate node) does not hit the network twice.
        self.local
            .lock()
            .expect("cache mutex")
            .insert(*result.chunk.address(), result.chunk.clone());
        Ok(result.chunk)
    }
}

/// A chunk getter that fetches over the network and re-races a transient
/// failure inline, surfacing an error only once every pass is exhausted.
///
/// The bounded-memory streaming download drives the core joiner's offset stream,
/// which fetches the chunk tree with bounded concurrency and reads through this
/// getter. Per-chunk resilience lives here so the joiner keeps a difficult
/// chunk's own concurrency slot busy re-racing it without gating the other
/// in-flight leaves, which keep resolving and being written to their offsets.
///
/// The core `ChunkGet::get` future must be `Send`, so this path uses no browser
/// macrotask yield or `setTimeout` timeout (both `!Send` on wasm): a congested
/// wave is recovered by immediate re-races, bounded by `max_passes`, and the
/// joiner's bounded concurrency caps the fan-out rather than a per-chunk
/// timeout. Each `retrieve_chunk` awaits real socket I/O, so the executor still
/// interleaves the in-flight legs at every await point.
#[derive(Clone)]
pub struct RetryingChunkGet {
    provider: Arc<dyn SwarmChunkProvider>,
    max_passes: u32,
}

impl RetryingChunkGet {
    /// Build a retrying getter over `provider` with the streaming download's
    /// inline re-race budget.
    pub fn new(provider: Arc<dyn SwarmChunkProvider>) -> Self {
        Self {
            provider,
            max_passes: 12,
        }
    }
}

impl ChunkGet<DEFAULT_BODY_SIZE> for RetryingChunkGet {
    type Error = ChunkStoreError;

    async fn get(&self, address: &ChunkAddress) -> Result<AnyChunk, Self::Error> {
        let mut last = String::new();
        for _ in 0..self.max_passes {
            match self.provider.retrieve_chunk(address).await {
                Ok(r) => return Ok(r.chunk),
                Err(e) => last = e.to_string(),
            }
        }
        Err(ChunkStoreError::Other(format!(
            "retrieve {address} exhausted retries: {last}"
        )))
    }
}
