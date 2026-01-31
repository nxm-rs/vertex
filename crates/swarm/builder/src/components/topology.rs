//! Topology builder trait and implementations.

use std::sync::Arc;

use vertex_swarm_kademlia::{KademliaConfig, KademliaTopology};
use vertex_swarm_api::{SwarmClientTypes, SwarmNetworkConfig, SwarmTopology};

use crate::SwarmBuilderContext;

/// Builds the topology component.
pub trait TopologyBuilder<Types: SwarmClientTypes, Cfg: SwarmNetworkConfig>: Send + Sync + 'static {
    /// The topology type produced (may be Arc-wrapped).
    type Topology: SwarmTopology + Send + Sync + 'static;

    /// Build the topology given the context.
    fn build_topology(self, ctx: &SwarmBuilderContext<'_, Types, Cfg>) -> Self::Topology;
}

/// Default Kademlia topology builder.
///
/// Produces `Arc<KademliaTopology<I>>` which implements `SwarmTopology`.
#[derive(Debug, Clone, Default)]
pub struct KademliaTopologyBuilder {
    config: KademliaConfig,
}

impl KademliaTopologyBuilder {
    /// Create with custom config.
    pub fn with_config(config: KademliaConfig) -> Self {
        Self { config }
    }
}

impl<Types, Cfg> TopologyBuilder<Types, Cfg> for KademliaTopologyBuilder
where
    Types: SwarmClientTypes,
    Types::Identity: Clone + Send + Sync + 'static,
    Cfg: SwarmNetworkConfig,
{
    type Topology = Arc<KademliaTopology<Types::Identity>>;

    fn build_topology(self, ctx: &SwarmBuilderContext<'_, Types, Cfg>) -> Self::Topology {
        let identity = (*ctx.identity).clone();
        let topology = KademliaTopology::new(identity, self.config);

        // Spawn the manage loop
        let _handle = topology.clone().spawn_manage_loop(&ctx.executor);

        topology
    }
}
