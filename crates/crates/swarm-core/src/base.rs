//! Light node implementation (base functionality for all node types)

use std::sync::Arc;

use async_trait::async_trait;
use dashmap::DashMap;
use tracing::{debug, error, info, trace, warn};
use vertex_primitives::{ChunkAddress, Error, Result};
use vertex_swarm_api::{
    chunk::Chunk,
    node::{NodeConfig, NodeMode, SwarmBaseNode},
    access::Credential,
    network::{NetworkClient, NetworkConfig, NetworkStatus},
    storage::ChunkStore,
};
use vertex_swarmspec::SwarmSpec;

#[cfg(feature = "network")]
use vertex_network::NetworkClientImpl;

#[cfg(feature = "access")]
use vertex_access::PostageStampCredential;

/// Components for a light node
pub(crate) struct LightNodeComponents<C: Credential> {
    /// Network client for communication
    network: Arc<dyn NetworkClient<Credential = C>>,

    /// Local cache for chunks
    cache: Arc<DashMap<ChunkAddress, Box<dyn Chunk>>>,
}

impl<C: Credential> LightNodeComponents<C> {
    /// Create new light node components
    pub(crate) async fn new(config: &NodeConfig, spec: Arc<dyn SwarmSpec>) -> Result<Self> {
        // Create network client
        #[cfg(feature = "network")]
        let network: Arc<dyn NetworkClient<Credential = C>> = Arc::new(
            NetworkClientImpl::new(&config.network, spec.clone())
                .await
                .map_err(|e| Error::network(format!("Failed to create network client: {}", e)))?
        );

        #[cfg(not(feature = "network"))]
        let network: Arc<dyn NetworkClient<Credential = C>> = {
            compile_error!("The 'network' feature must be enabled to create a node");
            unreachable!();
        };

        let cache = Arc::new(DashMap::new());

        Ok(Self {
            network,
            cache,
        })
    }

    /// Store a chunk in the network
    pub(crate) async fn store(&self, chunk: Box<dyn Chunk>, credential: &C) -> Result<()> {
        // Add to local cache first
        let address = chunk.address().clone();
        self.cache.insert(address.clone(), chunk.clone_box());

        // Then send to network
        self.network.store(chunk, credential).await
    }

    /// Retrieve a chunk from the network
    pub(crate) async fn retrieve(
        &self,
        address: &ChunkAddress,
        credential: Option<&C>,
    ) -> Result<Box<dyn Chunk>> {
        // Check local cache first
        if let Some(chunk) = self.cache.get(address) {
            debug!("Retrieved chunk {} from local cache", address);
            return Ok(chunk.clone_box());
        }

        // Retrieve from network
        let chunk = self.network.retrieve(address, credential).await?;

        // Add to cache
        self.cache.insert(address.clone(), chunk.clone_box());

        Ok(chunk)
    }

    /// Get current network status
    pub(crate) fn network_status(&self) -> NetworkStatus {
        self.network.status()
    }

    /// Connect to the network
    pub(crate) async fn connect(&self) -> Result<()> {
        self.network.connect().await
    }

    /// Disconnect from the network
    pub(crate) async fn disconnect(&self) -> Result<()> {
        self.network.disconnect().await
    }
}

/// Default light node implementation
#[cfg(feature = "access")]
pub struct LightNode {
    /// Core node implementation
    inner: Arc<SwarmNode<PostageStampCredential>>,
}

#[cfg(feature = "access")]
#[async_trait]
impl SwarmBaseNode for LightNode {
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
