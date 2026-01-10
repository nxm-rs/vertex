
//! Incentivized node implementation (extends full node functionality)

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::RwLock;
use tracing::{debug, error, info, trace, warn};
use vertex_primitives::{ChunkAddress, Error, Result};
use vertex_swarm_api::{
    chunk::Chunk,
    node::{IncentiveStatus, NodeConfig, NodeMode, SwarmBaseNode, SwarmFullNode, SwarmIncentivizedNode},
    access::Credential,
    network::NetworkStatus,
    storage::StorageStats,
};
use vertex_swarmspec::SwarmSpec;

use crate::{FullNodeComponents, SwarmNode};

#[cfg(feature = "access")]
use vertex_access::PostageStampCredential;

/// Components for an incentivized node (extends full node components)
pub(crate) struct IncentivizedNodeComponents<C: Credential> {
    /// Base full node components
    base: FullNodeComponents<C>,

    /// Incentive status
    incentive_status: RwLock<IncentiveStatus>,
}

impl<C: Credential> IncentivizedNodeComponents<C> {
    /// Create new incentivized node components
    pub(crate) async fn new(config: &NodeConfig, spec: Arc<dyn SwarmSpec>) -> Result<Self> {
        // Initialize full node components first
        let base = FullNodeComponents::new(config, spec.clone()).await?;

        // Initialize with default incentive status
        let incentive_status = RwLock::new(IncentiveStatus {
            is_registered: false,
            total_rewards: 0,
            unclaimed_rewards: 0,
            last_participation: 0,
            staking_status: None,
        });

        Ok(Self {
            base,
            incentive_status,
        })
    }

    /// Forward storage method to base
    pub(crate) async fn store(&self, chunk: Box<dyn Chunk>, credential: &C) -> Result<()> {
        self.base.store(chunk, credential).await
    }

    /// Forward retrieval method to base
    pub(crate) async fn retrieve(
        &self,
        address: &ChunkAddress,
        credential: Option<&C>,
    ) -> Result<Box<dyn Chunk>> {
        self.base.retrieve(address, credential).await
    }

    /// Forward network status method to base
    pub(crate) fn network_status(&self) -> NetworkStatus {
        self.base.network_status()
    }

    /// Forward connect method to base
    pub(crate) async fn connect(&self) -> Result<()> {
        self.base.connect().await
    }

    /// Forward disconnect method to base
    pub(crate) async fn disconnect(&self) -> Result<()> {
        self.base.disconnect().await
    }

    /// Forward is_responsible_for method to base
    pub(crate) fn is_responsible_for(&self, address: &ChunkAddress) -> Result<bool> {
        self.base.is_responsible_for(address)
    }

    /// Forward storage_stats method to base
    pub(crate) fn storage_stats(&self) -> Result<StorageStats> {
        self.base.storage_stats()
    }

    /// Forward sync_responsible_chunks method to base
    pub(crate) async fn sync_responsible_chunks(&self) -> Result<()> {
        self.base.sync_responsible_chunks().await
    }

    /// Forward recalculate_depth method to base
    pub(crate) async fn recalculate_depth(&self) -> Result<u8> {
        self.base.recalculate_depth().await
    }

    /// Forward neighborhood_depth method to base
    pub(crate) async fn neighborhood_depth(&self) -> u8 {
        self.base.neighborhood_depth().await
    }

    /// Register this node as a storage provider
    pub(crate) async fn register(&self) -> Result<()> {
        info!("Registering as storage provider");

        // In a real implementation, this would interact with blockchain contracts

        // Update status
        {
            let mut status = self.incentive_status.write().await;
            status.is_registered = true;
        }

        info!("Successfully registered as storage provider");

        Ok(())
    }

    /// Participate in redistribution lottery
    pub(crate) async fn participate_in_redistribution(&self) -> Result<()> {
        info!("Participating in redistribution lottery");

        // In a real implementation, this would:
        // 1. Generate proofs for chunks we're storing
        // 2. Submit those proofs to the redistribution contract
        // 3. Get results

        // Update status
        {
            let mut status = self.incentive_status.write().await;
            status.last_participation =
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
        }

        info!("Successfully participated in redistribution lottery");

        Ok(())
    }

    /// Claim earned rewards
    pub(crate) async fn claim_rewards(&self) -> Result<u64> {
        info!("Claiming rewards");

        // In a real implementation, this would interact with blockchain contracts

        // Update status
        let claimed_amount = {
            let mut status = self.incentive_status.write().await;
            let amount = status.unclaimed_rewards;
            status.unclaimed_rewards = 0;
            amount
        };

        info!("Successfully claimed {} rewards", claimed_amount);

        Ok(claimed_amount)
    }

    /// Get current incentive status
    pub(crate) async fn incentive_status(&self) -> Result<IncentiveStatus> {
        let status = self.incentive_status.read().await.clone();
        Ok(status)
    }
}

/// Default incentivized node implementation
#[cfg(feature = "access")]
pub struct IncentivizedNode {
    /// Core node implementation
    inner: Arc<SwarmNode<PostageStampCredential>>,
}

#[cfg(feature = "access")]
#[async_trait]
impl SwarmIncentivizedNode for IncentivizedNode {
    async fn register(&self) -> Result<()> {
        // Forward to inner implementation
        self.inner.components.register().await
    }

    async fn participate_in_redistribution(&self) -> Result<()> {
        // Forward to inner implementation
        self.inner.components.participate_in_redistribution().await
    }

    async fn claim_rewards(&self) -> Result<u64> {
        // Forward to inner implementation
        self.inner.components.claim_rewards().await
    }

    async fn incentive_status(&self) -> Result<IncentiveStatus> {
        // Forward to inner implementation
        self.inner.components.incentive_status().await
    }
}

#[cfg(feature = "access")]
#[async_trait]
impl SwarmFullNode for IncentivizedNode {
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
impl SwarmBaseNode for IncentivizedNode {
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
