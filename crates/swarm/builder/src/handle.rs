//! Build output for Swarm node types.

use std::sync::Arc;

use vertex_swarm_api::HasTopology;
use vertex_swarm_identity::Identity;
use vertex_tasks::{GracefulShutdown, NodeTask, NodeTaskFn};

use crate::providers::NetworkChunkProvider;
use crate::rpc::{BootnodeRpcProviders, ClientRpcProviders, StorerRpcProviders};

/// Build output from launching a Swarm node.
///
/// Contains the node's main event loop task function and RPC providers.
/// The providers type `P` determines RPC capabilities. All providers
/// implement `HasTopology` for topology access.
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
pub type BuiltBootnode = BuiltNode<BootnodeRpcProviders<Arc<Identity>>>;

/// Built client node (topology + chunk retrieval).
pub type BuiltClient =
    BuiltNode<ClientRpcProviders<Arc<Identity>, NetworkChunkProvider<Arc<Identity>>>>;

/// Built storer node (topology + chunks + storage).
pub type BuiltStorer =
    BuiltNode<StorerRpcProviders<Arc<Identity>, NetworkChunkProvider<Arc<Identity>>>>;
