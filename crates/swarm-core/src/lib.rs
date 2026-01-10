//! Core implementation of Swarm node functionality
//!
//! This crate provides the central implementation of the Swarm node,
//! integrating the various components (storage, network, access control, etc.)
//! into a cohesive whole.

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use dashmap::DashMap;
use tracing::{debug, error, info, trace, warn};
use vertex_primitives::{ChunkAddress, Error, Result};
use vertex_swarm_api::{
    chunk::{Chunk, ChunkFactory},
    node::{NodeConfig, NodeMode, SwarmBaseNode, SwarmFullNode, SwarmIncentivizedNode},
    access::Credential,
    network::NetworkStatus,
};
use vertex_swarmspec::SwarmSpec;

mod base;
mod full;
mod incentivized;
mod factory;
mod info;
mod config;

pub use base::*;
pub use full::*;
pub use incentivized::*;
pub use factory::*;
pub use info::*;
pub use config::*;

/// The core Swarm node implementation
///
/// This struct integrates all the components of a Swarm node and provides
/// the main entry point for node functionality.
pub struct SwarmNode<C: Credential> {
    /// Node configuration
    config: NodeConfig,

    /// Network specification
    spec: Arc<dyn SwarmSpec>,

    /// Node operating mode
    mode: NodeMode,

    /// Node components based on mode
    components: NodeComponents<C>,

    /// Timestamp of node startup
    start_time: std::time::Instant,
}

/// Node components that vary based on node mode
enum NodeComponents<C: Credential> {
    /// Light node components
    Light(LightNodeComponents<C>),

    /// Full node components
    Full(FullNodeComponents<C>),

    /// Incentivized node components
    Incentivized(IncentivizedNodeComponents<C>),
}

/// Implementation of core node functionality
impl<C: Credential> SwarmNode<C> {
    /// Create a new Swarm node with the given configuration and specification
    pub async fn new(config: NodeConfig, spec: Arc<dyn SwarmSpec>) -> Result<Self> {
        let mode = config.mode;
        let start_time = std::time::Instant::now();

        let components = match mode {
            NodeMode::Light => {
                info!("Initializing light node");
                let light_components = LightNodeComponents::new(&config, spec.clone()).await?;
                NodeComponents::Light(light_components)
            }
            NodeMode::Full => {
                info!("Initializing full node");
                let full_components = FullNodeComponents::new(&config, spec.clone()).await?;
                NodeComponents::Full(full_components)
            }
            NodeMode::Incentivized => {
                info!("Initializing incentivized node");
                let incentivized_components = IncentivizedNodeComponents::new(&config, spec.clone()).await?;
                NodeComponents::Incentivized(incentivized_components)
            }
        };

        Ok(Self {
            config,
            spec,
            mode,
            components,
            start_time,
        })
    }

    /// Get the node's uptime in seconds
    pub fn uptime(&self) -> u64 {
        self.start_time.elapsed().as_secs()
    }

    /// Return the network specification
    pub fn spec(&self) -> &dyn SwarmSpec {
        self.spec.as_ref()
    }

    /// Returns the current node configuration
    pub fn config(&self) -> &NodeConfig {
        &self.config
    }
}

#[async_trait]
impl<C: Credential> SwarmBaseNode for SwarmNode<C> {
    type Credential = C;

    async fn store(&self, chunk: Box<dyn Chunk>, credential: &Self::Credential) -> Result<()> {
        match &self.components {
            NodeComponents::Light(components) => components.store(chunk, credential).await,
            NodeComponents::Full(components) => components.store(chunk, credential).await,
            NodeComponents::Incentivized(components) => components.store(chunk, credential).await,
        }
    }

    async fn retrieve(
        &self,
        address: &ChunkAddress,
        credential: Option<&Self::Credential>,
    ) -> Result<Box<dyn Chunk>> {
        match &self.components {
            NodeComponents::Light(components) => components.retrieve(address, credential).await,
            NodeComponents::Full(components) => components.retrieve(address, credential).await,
            NodeComponents::Incentivized(components) => components.retrieve(address, credential).await,
        }
    }

    fn mode(&self) -> NodeMode {
        self.mode
    }

    fn network_status(&self) -> NetworkStatus {
        match &self.components {
            NodeComponents::Light(components) => components.network_status(),
            NodeComponents::Full(components) => components.network_status(),
            NodeComponents::Incentivized(components) => components.network_status(),
        }
    }

    async fn connect(&self) -> Result<()> {
        match &self.components {
            NodeComponents::Light(components) => components.connect().await,
            NodeComponents::Full(components) => components.connect().await,
            NodeComponents::Incentivized(components) => components.connect().await,
        }
    }

    async fn disconnect(&self) -> Result<()> {
        match &self.components {
            NodeComponents::Light(components) => components.disconnect().await,
            NodeComponents::Full(components) => components.disconnect().await,
            NodeComponents::Incentivized(components) => components.disconnect().await,
        }
    }
}
