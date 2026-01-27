//! Runnable services for Swarm nodes.
//!
//! These structs hold the services that will be consumed by [`Protocol::run()`].
//! Services are moved into spawned tasks and cannot be recovered after running.
//!
//! # Components vs Services
//!
//! - **Components**: Static data for RPC queries (identity, topology, accounting).
//!   Defined in [`crate::components`].
//! - **Services**: Runnable tasks (node event loop, client service). Defined here.
//!
//! # Unified Services
//!
//! All node types share the same service structure via [`SwarmServices`].
//! The capability level (Light, Publisher, Full) only affects components,
//! not services.
//!
//! [`Protocol::run()`]: vertex_node_api::Protocol::run

use async_trait::async_trait;

use crate::BootnodeTypes;

/// Trait for a runnable Swarm node event loop.
///
/// This is the main P2P networking task that handles peer connections,
/// protocol messages, and network events.
#[async_trait]
pub trait RunnableNode: Send + 'static {
    /// The error type returned if the node fails.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Run the node event loop.
    ///
    /// This method should run until shutdown is signaled or an error occurs.
    /// It handles:
    /// - Accepting incoming peer connections
    /// - Processing protocol messages
    /// - Managing peer state transitions
    async fn run(self) -> Result<(), Self::Error>;
}

/// Trait for a runnable client service.
///
/// The client service processes chunk requests from higher-level APIs.
#[async_trait]
pub trait RunnableClientService: Send + 'static {
    /// The error type returned if the service fails.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Run the client service event loop.
    ///
    /// This method should run until shutdown is signaled or an error occurs.
    async fn run(self) -> Result<(), Self::Error>;
}

/// Runnable services for any Swarm node.
///
/// All node types (Light, Publisher, Full) share the same service structure:
/// a node event loop and a client service. The capability level only affects
/// which components are available, not which services run.
///
/// # Type Parameters
///
/// Services are specified via the [`BootnodeTypes`] trait's associated types.
pub struct SwarmServices<Types: BootnodeTypes> {
    /// The Swarm node event loop.
    ///
    /// This is the main P2P networking task that handles peer connections,
    /// protocol messages, and network events.
    pub node: Types::Node,

    /// The client service for handling chunk requests.
    ///
    /// Processes retrieve requests from higher-level APIs.
    pub client_service: Types::ClientService,

    /// Handle to interact with the client service.
    ///
    /// Used to send requests to the client service after it's spawned.
    pub client_handle: Types::ClientHandle,
}

impl<Types: BootnodeTypes> SwarmServices<Types> {
    /// Create new services.
    pub fn new(
        node: Types::Node,
        client_service: Types::ClientService,
        client_handle: Types::ClientHandle,
    ) -> Self {
        Self {
            node,
            client_service,
            client_handle,
        }
    }

    /// Decompose into parts for spawning.
    pub fn into_parts(self) -> (Types::Node, Types::ClientService, Types::ClientHandle) {
        (self.node, self.client_service, self.client_handle)
    }
}
