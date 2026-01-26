//! Node builder type-state pattern.
//!
//! The builder follows a type-state pattern where each stage is a distinct type:
//!
//! ```text
//! NodeBuilder<P>
//!   │
//!   ├── with_launch_context(executor, dirs, api)
//!   ▼
//! WithLaunchContext<P>
//!   │
//!   ├── with_protocol(config)
//!   ▼
//! WithProtocol<P>
//!   │
//!   ├── launch()
//!   ▼
//! NodeHandle<P::Components>
//! ```
//!
//! # Example
//!
//! ```ignore
//! use vertex_node_builder::NodeBuilder;
//! use vertex_swarm_api::SwarmLightProtocol;
//!
//! let handle = NodeBuilder::<SwarmLightProtocol<MyConfig>>::new()
//!     .with_launch_context(executor, dirs, api_config)
//!     .with_protocol(protocol_config)
//!     .launch()
//!     .await?;
//!
//! handle.wait_for_exit().await?;
//! ```

use std::marker::PhantomData;
use std::net::{IpAddr, SocketAddr};

use vertex_node_api::{Built, NodeContext, Protocol, RpcConfig};
use vertex_node_core::dirs::DataDirs;
use vertex_rpc_server::GrpcRegistry;
use vertex_tasks::TaskExecutor;

use crate::NodeHandle;

/// Context for launching a node.
///
/// Contains all infrastructure needed to launch any node:
/// - Task executor for spawning background tasks
/// - Data directories for persistent storage
/// - API configuration (gRPC, metrics)
#[derive(Clone)]
pub struct LaunchContext<A = ()> {
    /// Task executor for spawning background tasks.
    pub executor: TaskExecutor,
    /// Data directories for this node.
    pub dirs: DataDirs,
    /// API configuration.
    pub api: A,
}

impl<A> LaunchContext<A> {
    /// Create a new launch context.
    pub fn new(executor: TaskExecutor, dirs: DataDirs, api: A) -> Self {
        Self { executor, dirs, api }
    }

    /// Get the data directory root.
    pub fn data_dir(&self) -> &std::path::PathBuf {
        &self.dirs.root
    }

    /// Create a node context from this launch context.
    ///
    /// The node context uses the network-specific directory as the data path.
    pub fn node_context(&self) -> NodeContext {
        NodeContext::new(self.executor.clone(), self.dirs.network.clone())
    }
}

impl<A: RpcConfig> LaunchContext<A> {
    /// Get the gRPC socket address from configuration.
    pub fn grpc_addr(&self) -> SocketAddr {
        let ip: IpAddr = self.api.grpc_addr().parse().unwrap_or([127, 0, 0, 1].into());
        SocketAddr::new(ip, self.api.grpc_port())
    }
}

/// Node builder - first stage.
///
/// The protocol type `P` determines what kind of node will be built.
/// Use [`with_launch_context`](Self::with_launch_context) to provide
/// infrastructure configuration.
pub struct NodeBuilder<P: Protocol> {
    _phantom: PhantomData<P>,
}

impl<P: Protocol> NodeBuilder<P> {
    /// Create a new node builder for the given protocol type.
    pub fn new() -> Self {
        Self {
            _phantom: PhantomData,
        }
    }

    /// Add launch context (executor, data directories, and API config).
    pub fn with_launch_context<A>(
        self,
        executor: TaskExecutor,
        dirs: DataDirs,
        api: A,
    ) -> WithLaunchContext<P, A> {
        WithLaunchContext {
            ctx: LaunchContext::new(executor, dirs, api),
            _phantom: PhantomData,
        }
    }
}

impl<P: Protocol> Default for NodeBuilder<P> {
    fn default() -> Self {
        Self::new()
    }
}

/// Builder with launch context attached.
///
/// Use [`with_protocol`](Self::with_protocol) to provide the protocol configuration.
pub struct WithLaunchContext<P: Protocol, A> {
    ctx: LaunchContext<A>,
    _phantom: PhantomData<P>,
}

impl<P: Protocol, A> WithLaunchContext<P, A> {
    /// Get a reference to the launch context.
    pub fn context(&self) -> &LaunchContext<A> {
        &self.ctx
    }

    /// Get the data directories.
    pub fn dirs(&self) -> &DataDirs {
        &self.ctx.dirs
    }

    /// Get the task executor.
    pub fn executor(&self) -> &TaskExecutor {
        &self.ctx.executor
    }

    /// Provide the protocol configuration.
    ///
    /// The configuration will be used to build the protocol components and services.
    pub fn with_protocol(self, config: P::Config) -> WithProtocol<P, A> {
        WithProtocol {
            ctx: self.ctx,
            config,
        }
    }
}

/// Builder with protocol configuration, ready to launch.
pub struct WithProtocol<P: Protocol, A> {
    ctx: LaunchContext<A>,
    config: P::Config,
}

impl<P: Protocol, A: RpcConfig> WithProtocol<P, A> {
    /// Get a reference to the launch context.
    pub fn context(&self) -> &LaunchContext<A> {
        &self.ctx
    }

    /// Launch the node.
    ///
    /// This builds the protocol, starts the gRPC server, and runs protocol services.
    /// The gRPC address is taken from the launch context's API configuration.
    pub async fn launch(self) -> Result<NodeHandle<P::Components>, P::BuildError> {
        let node_ctx = self.ctx.node_context();
        let grpc_addr = self.ctx.grpc_addr();

        // Build the protocol
        let built: Built<P> = P::build(self.config, &node_ctx).await?;

        // Create gRPC registry
        let registry = GrpcRegistry::new();

        // Build the gRPC server
        let grpc_handle = registry
            .into_server(grpc_addr)
            .expect("failed to build gRPC server");

        // Run the protocol services
        let components = built.run(&self.ctx.executor);

        Ok(NodeHandle::new(components, self.ctx.executor.clone(), grpc_handle))
    }
}

impl<P: Protocol> WithProtocol<P, ()> {
    /// Launch the node without gRPC server.
    ///
    /// Use this when you don't need the gRPC API.
    pub async fn launch_without_grpc(self) -> Result<NodeHandle<P::Components>, P::BuildError> {
        let node_ctx = self.ctx.node_context();

        // Build the protocol
        let built: Built<P> = P::build(self.config, &node_ctx).await?;

        // Create empty gRPC registry (no services)
        let registry = GrpcRegistry::new();
        let grpc_handle = registry
            .into_server(SocketAddr::new([127, 0, 0, 1].into(), 0))
            .expect("failed to build gRPC server");

        // Run the protocol services
        let components = built.run(&self.ctx.executor);

        Ok(NodeHandle::new(components, self.ctx.executor.clone(), grpc_handle))
    }
}
