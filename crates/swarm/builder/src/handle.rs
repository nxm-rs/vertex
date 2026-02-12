//! Generic node handle for all Swarm node types.

use std::sync::Arc;

use vertex_swarm_api::{HasTopology, NodeTask, NodeTaskFn};
use vertex_swarm_identity::Identity;
use vertex_tasks::GracefulShutdown;

use crate::providers::NetworkChunkProvider;
use crate::rpc::{BootnodeRpcProviders, ClientRpcProviders, StorerRpcProviders};

/// Handle returned from launching any Swarm node.
///
/// The providers type `P` determines RPC capabilities. All providers
/// implement `HasTopology` for topology access.
pub struct NodeHandle<P> {
    task_fn: NodeTaskFn,
    providers: P,
}

impl<P> NodeHandle<P> {
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
