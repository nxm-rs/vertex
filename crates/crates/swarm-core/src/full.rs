//! Full node implementation (extends light node functionality)

use std::sync::Arc;

use async_trait::async_trait;
use dashmap::DashMap;
use tokio::sync::Mutex;
use tracing::{debug, error, info, trace, warn};
use vertex_primitives::{ChunkAddress, Error, Result};
use vertex_swarm_api::{
    chunk::Chunk,
    node::{NodeConfig, NodeMode, SwarmBaseNode, SwarmFullNode},
    access::Credential,
    network::{NetworkClient, NetworkStatus},
    storage::{ChunkStore, StorageStats},
};
use vertex_swarmspec::SwarmSpec;

use crate::{LightNodeComponents, SwarmNode};

#[cfg(feature = "storage")]
use vertex_storage::DiskChunkStore;

#[cfg(feature = "access")]
use vertex_access::PostageStampCredential;

/// Components for a full node (extends light node components)
pub(crate) struct FullNodeComponents<C: Credential> {
    /// Base light node components
    base: LightNodeComponents<C>,

    /// Persistent storage for chunks
    store: Arc<dyn ChunkStore>,

    /// Current neighborhood depth
    depth: Mutex<u8>,
}

impl<C: Credential> FullNodeComponents<C> {
    /// Create new full node components
    pub(crate) async fn new(config: &NodeConfig, spec: Arc<dyn SwarmSpec>) -> Result<Self> {
        // Initialize light node components first
        let base = LightNodeComponents::new(config, spec.clone()).await?;

        // Initialize persistent storage
        #[cfg(feature = "storage")]
        let store: Arc<dyn ChunkStore> = Arc::new(
            DiskChunkStore::new(&config.storage)
                .map_err(|e| Error::storage(format!("Failed to create chunk store: {}", e)))?
        );

        #[cfg(not(feature = "storage"))]
        let store: Arc<dyn ChunkStore> = {
            compile_error!("The 'storage' feature must be enabled to create a full node");
            unreachable!();
        };

        // Start with default depth, will be recalculated later
        let depth = Mutex::new(0);

        Ok(Self {
            base,
            store,
            depth,
        })
    }

    /// Store a chunk in the storage and network
    pub(crate) async fn store(&self, chunk: Box<dyn Chunk>, credential: &C) -> Result<()> {
        // Store locally first
        self.store.put(chunk.clone_box(), credential).await?;

        // Then propagate to network
        self.base.store(chunk, credential).await
    }

    /// Retrieve a chunk from storage or network
    pub(crate) async fn retrieve(
        &self,
        address: &ChunkAddress,
        credential: Option<&C>,
    ) -> Result<Box<dyn Chunk>> {
        // Check local storage first
        if let Some(chunk) = self.store.get(address).await? {
            debug!("Retrieved chunk {} from local storage", address);
            return Ok(chunk);
        }

        // Fall back to network retrieval
        self.base.retrieve(address, credential).await
    }

    /// Get network status
    pub(crate) fn network_status(&self) -> NetworkStatus {
        self.base.network_status()
    }

    /// Connect to the network
    pub(crate) async fn connect(&self) -> Result<()> {
        let result = self.base.connect().await;

        // After connecting, recalculate depth
        if result.is_ok() {
            self.recalculate_depth().await?;
        }

        result
    }

    /// Disconnect from the network
    pub(crate) async fn disconnect(&self) -> Result<()> {
        self.base.disconnect().await
    }

    /// Check if this node is responsible for a chunk
    pub(crate) fn is_responsible_for(&self, address: &ChunkAddress) -> Result<bool> {
        // Get current depth
        let depth = *self.depth.try_lock().map_err(|_| Error::other("Failed to lock depth"))?;

        // Get network status to determine our address
        let status = self.network_status();

        // In a real implementation, we'd compute whether we're responsible
        // based on our address and the chunk address
        // For now, just return a placeholder

        Ok(false) // Placeholder
    }

    /// Get storage statistics
    pub(crate) fn storage_stats(&self) -> Result<StorageStats> {
        self.store.stats().await
    }

    /// Recalculate neighborhood depth based on network size
    pub(crate) async fn recalculate_depth(&self) -> Result<u8> {
        let status = self.network_status();

        // Calculate new depth based on network size
        // For example, depth = log2(network_size)
        let new_depth = (status.estimated_network_size as f64).log2() as u8;

        // Update depth
        let mut depth = self.depth.lock().await;
        if *depth != new_depth {
            info!("Neighborhood depth updated: {} -> {}", *depth, new_depth);
            *depth = new_depth;
        }

        Ok(*depth)
    }

    /// Get current neighborhood depth
    pub(crate) async fn neighborhood_depth(&self) -> u8 {
        *self.depth.lock().await
    }

    /// Synchronize chunks this node is responsible for
    pub(crate) async fn sync_responsible_chunks(&self) -> Result<()> {
        // This would involve:
        // 1. Determining which chunks we're responsible for
        // 2. Checking which we already have
        // 3. Fetching the ones we don't have

        info!("Starting sync of responsible chunks");

        // Implementation would depend on protocol details

        info!("Completed sync of responsible chunks");

        Ok(())
    }
}

/// Default full node implementation
#[cfg(feature = "access")]
pub struct FullNode {
    /// Core node implementation
    inner: Arc<SwarmNode<PostageStampCredential>>,
}

#[cfg(feature = "access")]
#[async_trait]
impl SwarmFullNode for FullNode {
    fn is_responsible_for(&self, address: &ChunkAddress) -> bool {
        // Forward to inner implementation
        // In actual code, this would need proper error handling
        self.inner.components.is_responsible_for(address).unwrap_or(false)
    }

    async fn storage_stats(&self) -> Result<StorageStats> {
        // Forward to inner implementation
        self.inner.components.storage_stats()
    }

    async fn sync_responsible_chunks(&self) -> Result<()> {
        // Forward to inner implementation
        self.inner.components.sync_responsible_chunks().await
    }

    fn neighborhood_depth(&self) -> u8 {
        // Forward to inner implementation
        // In actual code, this would need proper error handling
        self.inner.components.neighborhood_depth().await.unwrap_or(0)
    }

    async fn recalculate_depth(&self) -> Result<u8> {
        // Forward to inner implementation
        self.inner.components.recalculate_depth().await
    }
}

#[cfg(feature = "access")]
#[async_trait]
impl SwarmBaseNode for FullNode {
    type Credential = PostageStampCredential;

    async fn store(&self, chunk: Box<dyn Chunk>, credential: &Self::Credential) -> Result<()> {
        self.inner.store(chunk, credential).await
    }

    async fn retrieve(
        &self,
        address: &ChunkAddress,
        credential: Option<&Self::Credential>,
    ) -> Result<Box<dyn Chunk>> {
        self.inner.retrieve(address, credential).await
    }

    fn mode(&self) -> NodeMode {
        self.inner.mode()
    }

    fn network_status(&self) -> NetworkStatus {
        self.inner.network_status()
    }

    async fn connect(&self) -> Result<()> {
        self.inner.connect().await
    }

    async fn disconnect(&self) -> Result<()> {
        self.inner.disconnect().await
    }
}
