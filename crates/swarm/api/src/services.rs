//! Services for Swarm nodes.
//!
//! These structs hold the services that will be spawned as background tasks.
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

use crate::BootnodeTypes;
use std::future::Future;

/// A service that can be spawned as a background task.
///
/// This is the minimal trait for types that can be run via `TaskExecutor::spawn_critical()`.
/// Implementors should provide their main event loop in `spawn_task()`.
pub trait SpawnableTask: Send + 'static {
    /// Run the service as a spawnable future, consuming self.
    ///
    /// The returned future runs until completion (shutdown or error).
    /// Errors are logged internally - the future always completes with `()`.
    fn spawn_task(self) -> impl Future<Output = ()> + Send;
}

/// Services for any Swarm node.
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
