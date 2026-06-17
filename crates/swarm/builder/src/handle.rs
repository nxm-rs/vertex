//! Build output for Swarm node types.

use std::sync::Arc;

use vertex_swarm_api::{
    BootnodeComponents, ClientComponents, HasTopology, StorerComponents, SwarmLocalStore,
};
use vertex_swarm_identity::Identity;
use vertex_swarm_topology::TopologyHandle;
use vertex_tasks::{GracefulShutdown, NodeTask, NodeTaskFn};

use crate::providers::NetworkChunkProvider;
use crate::verify::VerifyingChunkProvider;

/// Network chunk provider wrapped with config-gated download verification.
type VerifiedChunkProvider = VerifyingChunkProvider<NetworkChunkProvider<Arc<Identity>>>;

/// Build output from launching a Swarm node.
///
/// Contains the node's main event loop task function and the api component
/// container. The component type `P` determines the node's capabilities; all
/// containers implement `HasTopology` for topology access. The transport (gRPC
/// today) is wired at `bin/vertex`, not here.
pub struct BuiltNode<P> {
    task_fn: NodeTaskFn,
    providers: P,
}

impl<P> BuiltNode<P> {
    pub fn new(task_fn: NodeTaskFn, providers: P) -> Self {
        Self { task_fn, providers }
    }

    /// Consume and create the main event loop task with graceful shutdown.
    pub fn into_task(self, shutdown: GracefulShutdown) -> NodeTask {
        (self.task_fn)(shutdown)
    }

    pub fn providers(&self) -> &P {
        &self.providers
    }

    pub fn providers_mut(&mut self) -> &mut P {
        &mut self.providers
    }

    pub fn into_providers(self) -> P {
        self.providers
    }

    /// Decompose into task function and providers.
    pub fn into_parts(self) -> (NodeTaskFn, P) {
        (self.task_fn, self.providers)
    }
}

impl<P: HasTopology> BuiltNode<P> {
    /// Access the topology handle.
    pub fn topology(&self) -> &P::Topology {
        self.providers.topology()
    }
}

/// Built bootnode (topology only).
pub type BuiltBootnode = BuiltNode<BootnodeComponents<TopologyHandle<Arc<Identity>>>>;

/// Built client node (topology + chunk retrieval).
pub type BuiltClient =
    BuiltNode<ClientComponents<TopologyHandle<Arc<Identity>>, VerifiedChunkProvider>>;

/// Built storer node (topology + chunks + the persisting reserve as the local
/// store). The store is erased to `Arc<dyn SwarmLocalStore>`: the launch path
/// keeps the concrete reserve and shares one trait handle with the node so
/// serving-on-retrieval reads the reserve.
pub type BuiltStorer = BuiltNode<
    StorerComponents<
        TopologyHandle<Arc<Identity>>,
        VerifiedChunkProvider,
        Arc<dyn SwarmLocalStore>,
    >,
>;
