//! Unified client for Swarm nodes.

use async_trait::async_trait;
use nectar_primitives::{AnyChunk, ChunkAddress};
use vertex_swarm_api::{
    BootnodeComponents, ClientComponents, HasAccounting, HasTopology, SwarmClient,
    SwarmClientAccounting, SwarmError, SwarmResult, SwarmTopologyRouting,
};

use crate::ClientHandle;

/// Unified client for all Swarm node types.
///
/// Generic over component type `C`:
/// - [`BootnodeComponents<T>`] for bootnodes (topology only)
/// - [`ClientComponents<T, A>`] for client/storer nodes (topology + accounting)
pub struct Client<C, S = ()> {
    components: C,
    client_handle: ClientHandle,
    _storage: std::marker::PhantomData<S>,
}

impl<C, S> Client<C, S> {
    /// Create a client from components.
    pub fn new(components: C, client_handle: ClientHandle) -> Self {
        Self {
            components,
            client_handle,
            _storage: std::marker::PhantomData,
        }
    }

    /// Get the client handle.
    pub fn client_handle(&self) -> &ClientHandle {
        &self.client_handle
    }

    /// Get the components.
    pub fn components(&self) -> &C {
        &self.components
    }
}

impl<C: HasTopology, S> Client<C, S> {
    /// Get the topology.
    pub fn topology(&self) -> &C::Topology {
        self.components.topology()
    }
}

impl<C: HasAccounting, S> Client<C, S> {
    /// Get the accounting.
    pub fn accounting(&self) -> &C::Accounting {
        self.components.accounting()
    }
}

// Bootnode constructors
impl<T> Client<BootnodeComponents<T>, ()> {
    /// Create a bootnode client (topology only).
    pub fn bootnode(topology: T, client_handle: ClientHandle) -> Self {
        Self::new(BootnodeComponents::new(topology), client_handle)
    }
}

// Client constructors
impl<T, A> Client<ClientComponents<T, A>, ()> {
    /// Create a client node (topology + accounting).
    pub fn client(topology: T, accounting: A, client_handle: ClientHandle) -> Self {
        Self::new(ClientComponents::new(topology, accounting), client_handle)
    }
}

#[async_trait]
impl<T, A, S> SwarmClient for Client<ClientComponents<T, A>, S>
where
    T: SwarmTopologyRouting + Send + Sync + 'static,
    A: SwarmClientAccounting + Send + Sync + 'static,
    S: Send + Sync + 'static,
{
    type Storage = S;

    async fn get(&self, address: &ChunkAddress) -> SwarmResult<AnyChunk> {
        let _closest = self.components.topology().closest_to(address, 3);
        let _handle = &self.client_handle;
        let _accounting = self.components.accounting();

        // TODO: Retrieval implementation
        Err(SwarmError::ChunkNotFound { address: *address })
    }

    async fn put(&self, chunk: AnyChunk, _storage: &Self::Storage) -> SwarmResult<()> {
        let _closest = self.components.topology().closest_to(chunk.address(), 3);
        let _handle = &self.client_handle;
        let _accounting = self.components.accounting();

        // TODO: Push sync implementation
        Ok(())
    }
}

/// Bootnode client (topology only).
pub type BootnodeClient<T> = Client<BootnodeComponents<T>, ()>;

/// Full client (topology + accounting).
pub type FullClient<T, A, S = ()> = Client<ClientComponents<T, A>, S>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClientCommand, ClientHandle};
    use vertex_swarm_bandwidth::{Accounting, FixedPricer};
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use vertex_swarm_api::{SwarmBandwidthAccounting, SwarmTopologyRouting};
    use vertex_swarm_bandwidth::{ClientAccounting, DefaultBandwidthConfig};
    use vertex_swarm_test_utils::{test_identity_arc as test_identity, MockTopology};

    fn create_test_handle() -> ClientHandle {
        let (tx, _rx) = mpsc::channel::<ClientCommand>(16);
        ClientHandle::new(tx)
    }

    #[test]
    fn test_bootnode_client() {
        let topology = MockTopology::default();
        let handle = create_test_handle();

        let client = Client::bootnode(topology, handle);
        let _ = client.topology().neighbors(0);
    }

    #[test]
    fn test_full_client() {
        let topology = MockTopology::default();
        let bandwidth = Arc::new(Accounting::new(DefaultBandwidthConfig::default(), test_identity()));
        let pricer = FixedPricer::new(10_000, vertex_swarm_spec::init_mainnet());
        let accounting = ClientAccounting::new(bandwidth, pricer);
        let handle = create_test_handle();

        let client: FullClient<MockTopology, ClientAccounting<_, _>> =
            Client::client(topology, accounting, handle);

        let peers = SwarmBandwidthAccounting::peers(client.accounting().bandwidth());
        assert!(peers.is_empty());
    }
}
