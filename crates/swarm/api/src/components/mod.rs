//! Swarm node components - traits and runtime containers.

mod bandwidth;
mod pricing;
mod store;
mod topology;

pub use bandwidth::*;
pub use pricing::*;
pub use store::*;
pub use topology::*;

use crate::{SwarmBootnodeTypes, SwarmClientTypes, SwarmStorerTypes};

/// Base components container for all Swarm nodes.
///
/// Contains the fundamental components needed for network participation.
#[derive(Clone)]
pub struct BaseComponents<Types: SwarmBootnodeTypes>
where
    Types::Identity: Clone,
    Types::Topology: Clone,
{
    /// The node's cryptographic identity.
    pub identity: Types::Identity,

    /// Network topology manager (peer discovery, routing).
    pub topology: Types::Topology,
}

impl<Types: SwarmBootnodeTypes> BaseComponents<Types>
where
    Types::Identity: Clone,
    Types::Topology: Clone,
{
    /// Create new base components.
    pub fn new(identity: Types::Identity, topology: Types::Topology) -> Self {
        Self { identity, topology }
    }
}

/// Runtime components container for a built client Swarm node.
///
/// Client nodes can retrieve and upload chunks but don't store them locally.
/// Composes base components with bandwidth accounting.
#[derive(Clone)]
pub struct ClientComponents<Types: SwarmClientTypes>
where
    Types::Identity: Clone,
    Types::Topology: Clone,
    Types::Accounting: Clone,
    Types::ClientHandle: Clone,
{
    /// Base components (identity, topology).
    pub base: BaseComponents<Types>,

    /// Bandwidth accounting for availability incentives.
    pub accounting: Types::Accounting,

    /// Handle to the client service for network operations.
    ///
    /// Used for chunk retrieval via RPC.
    pub client_handle: Types::ClientHandle,
}

impl<Types: SwarmClientTypes> ClientComponents<Types>
where
    Types::Identity: Clone,
    Types::Topology: Clone,
    Types::Accounting: Clone,
    Types::ClientHandle: Clone,
{
    /// Create new client node components.
    pub fn new(
        identity: Types::Identity,
        topology: Types::Topology,
        accounting: Types::Accounting,
        client_handle: Types::ClientHandle,
    ) -> Self {
        Self {
            base: BaseComponents::new(identity, topology),
            accounting,
            client_handle,
        }
    }

    /// Get the node's identity.
    pub fn identity(&self) -> &Types::Identity {
        &self.base.identity
    }

    /// Get the topology manager.
    pub fn topology(&self) -> &Types::Topology {
        &self.base.topology
    }

    /// Get the client handle.
    pub fn client_handle(&self) -> &Types::ClientHandle {
        &self.client_handle
    }
}

/// Runtime components container for a built storer Swarm node.
///
/// Storer nodes store chunks locally.
/// Composes client components with local store.
#[derive(Clone)]
pub struct StorerComponents<Types: SwarmStorerTypes>
where
    Types::Identity: Clone,
    Types::Topology: Clone,
    Types::Accounting: Clone,
    Types::ClientHandle: Clone,
    Types::Store: Clone,
{
    /// Client node components (base + accounting).
    pub client: ClientComponents<Types>,

    /// Local chunk storage.
    pub store: Types::Store,
}

impl<Types: SwarmStorerTypes> StorerComponents<Types>
where
    Types::Identity: Clone,
    Types::Topology: Clone,
    Types::Accounting: Clone,
    Types::ClientHandle: Clone,
    Types::Store: Clone,
{
    /// Create new storer node components.
    pub fn new(
        identity: Types::Identity,
        topology: Types::Topology,
        accounting: Types::Accounting,
        client_handle: Types::ClientHandle,
        store: Types::Store,
    ) -> Self {
        Self {
            client: ClientComponents::new(identity, topology, accounting, client_handle),
            store,
        }
    }

    /// Get the node's identity.
    pub fn identity(&self) -> &Types::Identity {
        self.client.identity()
    }

    /// Get the topology manager.
    pub fn topology(&self) -> &Types::Topology {
        self.client.topology()
    }

    /// Get the accounting manager.
    pub fn accounting(&self) -> &Types::Accounting {
        &self.client.accounting
    }

    /// Get the client handle.
    pub fn client_handle(&self) -> &Types::ClientHandle {
        self.client.client_handle()
    }
}
