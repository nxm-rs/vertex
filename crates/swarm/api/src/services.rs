//! Services for Swarm nodes - background tasks that are spawned and run.

use crate::SwarmBootnodeTypes;

/// Services container for any Swarm node.
///
/// All node types share the same service structure.
pub struct Services<Types: SwarmBootnodeTypes> {
    /// The Swarm node event loop (P2P networking).
    pub node: Types::Node,

    /// The client service for handling chunk requests.
    pub client_service: Types::ClientService,

    /// Handle to interact with the client service.
    pub client_handle: Types::ClientHandle,
}

impl<Types: SwarmBootnodeTypes> Services<Types> {
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
