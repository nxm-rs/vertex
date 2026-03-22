//! Swarm build configuration.
//!
//! [`SwarmBuildConfig`] holds the raw protocol inputs needed to progressively
//! build a swarm node. Validation happens inside the builder chain (in the
//! [`SwarmLaunchConfig`] implementation), not up front.

use std::path::PathBuf;
use std::sync::Arc;

use vertex_node_api::NodeBuildsProtocol;
use vertex_swarm_api::{SwarmNodeType, SwarmProtocol};
use vertex_swarm_bandwidth::BandwidthConfigError;
use vertex_swarm_node::ProtocolConfig;
use vertex_swarm_spec::Spec;

use vertex_swarm_api::ConfigError;

/// Swarm build configuration: raw protocol inputs for progressive building.
///
/// Pass this to `with_protocol()` in the node builder pipeline. Validation
/// and assembly happen progressively inside the builder chain when
/// `SwarmLaunchConfig::build()` is called.
pub struct SwarmBuildConfig {
    pub(crate) protocol: ProtocolConfig,
    pub(crate) spec: Arc<Spec>,
    pub(crate) network_dir: PathBuf,
}

impl SwarmBuildConfig {
    pub fn new(protocol: ProtocolConfig, spec: Arc<Spec>, network_dir: PathBuf) -> Self {
        Self {
            protocol,
            spec,
            network_dir,
        }
    }
}

impl NodeBuildsProtocol for SwarmBuildConfig {
    type Protocol = SwarmProtocol<Self>;

    fn protocol_name(&self) -> &'static str {
        match self.protocol.node_type {
            SwarmNodeType::Bootnode => "Swarm Bootnode",
            SwarmNodeType::Client => "Swarm Client",
            SwarmNodeType::Storer => "Swarm Storer",
        }
    }
}

/// Error during swarm node build.
#[derive(Debug, thiserror::Error)]
pub enum SwarmConfigError {
    /// Network configuration is invalid.
    #[error("network config: {0}")]
    Network(#[from] ConfigError),
    /// Identity loading failed.
    #[error("identity: {0}")]
    Identity(#[source] eyre::Error),
    /// Bandwidth configuration is invalid.
    #[error("bandwidth config: {0}")]
    Bandwidth(#[from] BandwidthConfigError),
}
