//! Generic node handle for all Swarm node types.

use std::sync::Arc;

use vertex_swarm_api::{HasTopology, NodeTask};
use vertex_swarm_identity::Identity;

use crate::providers::NetworkChunkProvider;
use crate::rpc::{BootnodeRpcProviders, ClientRpcProviders, StorerRpcProviders};

/// Handle returned from launching any Swarm node.
///
/// The providers type `P` determines RPC capabilities. All providers
/// implement `HasTopology` for topology access.
pub struct NodeHandle<P> {
    task: NodeTask,
    providers: P,
}

impl<P> NodeHandle<P> {
    pub fn new(task: NodeTask, providers: P) -> Self {
        Self { task, providers }
    }

    /// Consume and return the main event loop task.
    pub fn into_task(self) -> NodeTask {
        self.task
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

    /// Decompose into task and providers.
    pub fn into_parts(self) -> (NodeTask, P) {
        (self.task, self.providers)
    }
}

impl<P: HasTopology> NodeHandle<P> {
    /// Access the topology handle.
    pub fn topology(&self) -> &P::Topology {
        self.providers.topology()
    }
}

/// Handle for bootnode (topology only).
pub type BootnodeHandle = NodeHandle<BootnodeRpcProviders<Arc<Identity>>>;

/// Handle for client node (topology + chunk retrieval).
pub type ClientHandle =
    NodeHandle<ClientRpcProviders<Arc<Identity>, NetworkChunkProvider<Arc<Identity>>>>;

/// Handle for storer node (topology + storage).
pub type StorerHandle = NodeHandle<StorerRpcProviders<Arc<Identity>>>;
