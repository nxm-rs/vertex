//! Build output for Swarm node types.

use std::sync::Arc;

use vertex_swarm_api::{
    BinCursorStore, BootnodeComponents, ClientComponents, HasTopology, StorerComponents,
    SwarmLocalStore,
};
use vertex_swarm_identity::Identity;
use vertex_swarm_topology::TopologyHandle;
use vertex_tasks::{GracefulShutdown, NodeTask, NodeTaskFn};

use vertex_swarm_node::NativeChunkProvider;

/// Build output from launching a Swarm node: the event-loop task function plus
/// the component container `P`, which determines the node's capabilities. All
/// containers implement `HasTopology`. The transport is wired at `bin/vertex`.
pub struct BuiltNode<P> {
    task_fn: NodeTaskFn,
    providers: P,
}

impl<P> BuiltNode<P> {
    pub fn new(task_fn: NodeTaskFn, providers: P) -> Self {
        Self { task_fn, providers }
    }

    /// Build the event-loop task wired to the shutdown signal.
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

    pub fn into_parts(self) -> (NodeTaskFn, P) {
        (self.task_fn, self.providers)
    }
}

impl<P: HasTopology> BuiltNode<P> {
    pub fn topology(&self) -> &P::Topology {
        self.providers.topology()
    }
}

/// Built bootnode (topology only).
pub type BuiltBootnode = BuiltNode<BootnodeComponents<TopologyHandle<Arc<Identity>>>>;

/// Built client node (topology + chunk retrieval).
pub type BuiltClient =
    BuiltNode<ClientComponents<TopologyHandle<Arc<Identity>>, NativeChunkProvider>>;

/// Built storer node (topology + chunks + retrieval-serve store + reserve).
///
/// Store and reserve axes are erased to `Arc<dyn SwarmLocalStore>` and
/// `Arc<dyn BinCursorStore>` so one handle each is shared with the run loop.
pub type BuiltStorer = BuiltNode<
    StorerComponents<
        TopologyHandle<Arc<Identity>>,
        NativeChunkProvider,
        Arc<dyn SwarmLocalStore>,
        Arc<dyn BinCursorStore>,
    >,
>;
