//! Node startup and shutdown

use crate::{builder::BuilderContext, FullNodeComponents, NodeTypes};
use vertex_primitives::Result;
use vertex_swarm_api::{NetworkConfig, StorageConfig};
use vertex_swarmspec::SwarmSpec;

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, string::String, vec::Vec};

/// A trait for node lifecycles
#[async_trait]
#[auto_impl::auto_impl(&, Arc)]
pub trait NodeLifecycle: Send + Sync + 'static {
    /// Initialize the node
    async fn initialize(&self) -> Result<()>;

    /// Start the node
    async fn start(&self) -> Result<()>;

    /// Stop the node
    async fn stop(&self) -> Result<()>;

    /// Shutdown the node
    async fn shutdown(&self) -> Result<()>;

    /// Restart the node
    async fn restart(&self) -> Result<()> {
        self.stop().await?;
        self.start().await
    }

    /// Check if the node is running
    fn is_running(&self) -> bool;
}

/// Factory for creating nodes
#[async_trait]
#[auto_impl::auto_impl(&, Arc)]
pub trait NodeFactory<N: NodeTypes>: Send + Sync + 'static {
    /// The node type this factory creates
    type Node: FullNodeComponents<Types = N>;

    /// Create a new node
    async fn create_node(
        &self,
        context: &BuilderContext<N>,
    ) -> Result<Self::Node>;
}

/// A launcher for a node
#[async_trait]
#[auto_impl::auto_impl(&, Arc)]
pub trait NodeLauncher<N: NodeTypes>: Send + Sync + 'static {
    /// The node type this launcher launches
    type Node: FullNodeComponents<Types = N>;

    /// Launch a node with the given node factory
    async fn launch_node<F: NodeFactory<N, Node = Self::Node>>(
        &self,
        factory: F,
        spec: N::Spec,
        network_config: NetworkConfig,
        storage_config: StorageConfig,
    ) -> Result<NodeHandle<Self::Node>>;
}

/// A handle to a launched node
pub struct NodeHandle<Node: FullNodeComponents> {
    /// The node
    pub node: Node,
    /// The node's lifecycle
    pub lifecycle: Box<dyn NodeLifecycle>,
    /// Exit future that resolves when the node exits
    pub exit_future: crate::exit::ExitFuture,
}

impl<Node: FullNodeComponents> NodeHandle<Node> {
    /// Create a new node handle
    pub fn new(
        node: Node,
        lifecycle: Box<dyn NodeLifecycle>,
        exit_future: crate::exit::ExitFuture,
    ) -> Self {
        Self {
            node,
            lifecycle,
            exit_future,
        }
    }

    /// Wait for the node to exit
    pub async fn wait_for_exit(self) -> Result<()> {
        self.exit_future.await
    }

    /// Stop the node
    pub async fn stop(&self) -> Result<()> {
        self.lifecycle.stop().await
    }

    /// Restart the node
    pub async fn restart(&self) -> Result<()> {
        self.lifecycle.restart().await
    }

    /// Shutdown the node
    pub async fn shutdown(self) -> Result<()> {
        self.lifecycle.shutdown().await
    }

    /// Check if the node is running
    pub fn is_running(&self) -> bool {
        self.lifecycle.is_running()
    }
}
