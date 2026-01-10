//! Factory for creating Swarm node instances

use std::sync::Arc;

use tracing::info;
use vertex_primitives::Result;
use vertex_swarm_api::{
    node::{NodeConfig, NodeMode, SwarmBaseNode, SwarmFullNode, SwarmIncentivizedNode},
};
use vertex_swarmspec::SwarmSpec;

#[cfg(feature = "access")]
use vertex_access::PostageStampCredential;

use crate::{
    SwarmNode,
    LightNode,
    FullNode,
    IncentivizedNode,
};

/// Factory for creating Swarm node instances
#[derive(Clone)]
pub struct SwarmNodeFactory {
    /// Default network specification to use
    default_spec: Arc<dyn SwarmSpec>,
}

impl SwarmNodeFactory {
    /// Create a new node factory with default network specification
    pub fn new(default_spec: Arc<dyn SwarmSpec>) -> Self {
        Self { default_spec }
    }

    /// Create a node with the given configuration
    pub async fn create_node(&self, config: NodeConfig) -> Result<Box<dyn SwarmBaseNode<Credential = PostageStampCredential>>> {
        match config.mode {
            NodeMode::Light => self.create_light_node(config).await,
            NodeMode::Full => self.create_full_node(config).await,
            NodeMode::Incentivized => self.create_incentivized_node(config).await,
        }
    }

    /// Create a light node
    pub async fn create_light_node(&self, config: NodeConfig) -> Result<Box<dyn SwarmBaseNode<Credential = PostageStampCredential>>> {
        info!("Creating light node");

        #[cfg(all(feature = "access", feature = "network"))]
        {
            let node = SwarmNode::<PostageStampCredential>::new(config, self.default_spec.clone()).await?;
            let light_node = LightNode {
                inner: Arc::new(node),
            };
            Ok(Box::new(light_node))
        }

        #[cfg(not(all(feature = "access", feature = "network")))]
        {
            Err(vertex_primitives::Error::other(
                "Cannot create light node: required features 'access' and 'network' are not enabled"
            ))
        }
    }

    /// Create a full node
    pub async fn create_full_node(&self, config: NodeConfig) -> Result<Box<dyn SwarmFullNode<Credential = PostageStampCredential>>> {
        info!("Creating full node");

        #[cfg(all(feature = "access", feature = "network", feature = "storage"))]
        {
            let node = SwarmNode::<PostageStampCredential>::new(config, self.default_spec.clone()).await?;
            let full_node = FullNode {
                inner: Arc::new(node),
            };
            Ok(Box::new(full_node))
        }

        #[cfg(not(all(feature = "access", feature = "network", feature = "storage")))]
        {
            Err(vertex_primitives::Error::other(
                "Cannot create full node: required features 'access', 'network', and 'storage' are not enabled"
            ))
        }
    }

    /// Create an incentivized node
    pub async fn create_incentivized_node(&self, config: NodeConfig) -> Result<Box<dyn SwarmIncentivizedNode<Credential = PostageStampCredential>>> {
        info!("Creating incentivized node");

        #[cfg(all(feature = "access", feature = "network", feature = "storage"))]
        {
            let node = SwarmNode::<PostageStampCredential>::new(config, self.default_spec.clone()).await?;
            let incentivized_node = IncentivizedNode {
                inner: Arc::new(node),
            };
            Ok(Box::new(incentivized_node))
        }

        #[cfg(not(all(feature = "access", feature = "network", feature = "storage")))]
        {
            Err(vertex_primitives::Error::other(
                "Cannot create incentivized node: required features 'access', 'network', and 'storage' are not enabled"
            ))
        }
    }
}
