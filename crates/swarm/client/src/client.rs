//! Client implementation.
//!
//! Provides a unified client for all node types:
//! - Bootnodes: topology only
//! - Client/Storer nodes: + accounting + pricer, implements [`SwarmClient`]

use std::sync::Arc;

use async_trait::async_trait;
use nectar_primitives::{AnyChunk, ChunkAddress};
use vertex_swarm_api::{
    SwarmBootnodeTypes, SwarmClientTypes, SwarmClient, SwarmError, SwarmResult, SwarmTopology,
};

use crate::ClientHandle;

/// Built components ready for use by Client.
///
/// Contains the topology, accounting, and pricer components.
#[derive(Debug, Clone)]
pub struct BuiltSwarmComponents<T, A, P> {
    /// The topology.
    pub topology: T,
    /// The accounting.
    pub accounting: A,
    /// The pricer.
    pub pricer: P,
}

// =============================================================================
// Client
// =============================================================================

/// Unified client for all node types.
///
/// # Type Parameters
///
/// - `Types`: Node capability level ([`SwarmBootnodeTypes`], [`SwarmClientTypes`], etc.)
/// - `C`: Components - `()` for bootnodes, `BuiltSwarmComponents<T, A, P>` for Client/Storer nodes
/// - `S`: Storage proof type - `()` by default, postage stamp for mainnet
///
/// # Examples
///
/// ```ignore
/// // Bootnode (peer discovery only)
/// let client = Client::<MyTypes>::bootnode(topology, handle);
///
/// // Client node (can retrieve and upload chunks)
/// let client = Client::full(topology, accounting, pricer, handle);
/// let chunk = client.get(&address).await?;
/// client.put(chunk, &stamp).await?;
/// ```
pub struct Client<Types: SwarmBootnodeTypes, C = (), S = ()> {
    topology: Arc<Types::Topology>,
    components: C,
    client_handle: ClientHandle,
    _storage: std::marker::PhantomData<S>,
}

// =============================================================================
// Bootnode (no components)
// =============================================================================

impl<Types: SwarmBootnodeTypes> Client<Types, (), ()> {
    /// Create a bootnode client (topology only, no accounting).
    pub fn bootnode(topology: Types::Topology, client_handle: ClientHandle) -> Self {
        Self {
            topology: Arc::new(topology),
            components: (),
            client_handle,
            _storage: std::marker::PhantomData,
        }
    }

    /// Create a bootnode client from Arc-wrapped topology.
    pub fn bootnode_from_arc(topology: Arc<Types::Topology>, client_handle: ClientHandle) -> Self {
        Self {
            topology,
            components: (),
            client_handle,
            _storage: std::marker::PhantomData,
        }
    }
}

// =============================================================================
// Client/Storer node (with components)
// =============================================================================

impl<Types, A, P, S> Client<Types, BuiltSwarmComponents<Types::Topology, A, P>, S>
where
    Types: SwarmClientTypes,
    Types::Topology: Clone,
    S: Send + Sync + 'static,
{
    /// Create a client from pre-built components.
    pub fn from_components(
        components: BuiltSwarmComponents<Types::Topology, A, P>,
        client_handle: ClientHandle,
    ) -> Self {
        Self {
            topology: Arc::new(components.topology.clone()),
            components,
            client_handle,
            _storage: std::marker::PhantomData,
        }
    }
}

impl<Types, A, P, S> Client<Types, BuiltSwarmComponents<Types::Topology, Arc<A>, P>, S>
where
    Types: SwarmClientTypes,
    Types::Topology: Clone,
    S: Send + Sync + 'static,
{
    /// Create a full client (can retrieve and publish chunks).
    ///
    /// Wraps `accounting` in `Arc` for cheap cloning.
    pub fn full(
        topology: Types::Topology,
        accounting: A,
        pricer: P,
        client_handle: ClientHandle,
    ) -> Self {
        Self {
            topology: Arc::new(topology.clone()),
            components: BuiltSwarmComponents {
                topology,
                accounting: Arc::new(accounting),
                pricer,
            },
            client_handle,
            _storage: std::marker::PhantomData,
        }
    }
}

// =============================================================================
// Component accessors (for clients with components)
// =============================================================================

impl<Types, A, P, S> Client<Types, BuiltSwarmComponents<Types::Topology, A, P>, S>
where
    Types: SwarmClientTypes,
{
    /// Get the accounting.
    pub fn accounting(&self) -> &A {
        &self.components.accounting
    }

    /// Get the pricer.
    pub fn pricer(&self) -> &P {
        &self.components.pricer
    }
}

// =============================================================================
// Common methods
// =============================================================================

impl<Types: SwarmBootnodeTypes, C, S> Client<Types, C, S> {
    /// Get the topology.
    pub fn topology(&self) -> &Types::Topology {
        &self.topology
    }

    /// Get the client handle.
    pub fn client_handle(&self) -> &ClientHandle {
        &self.client_handle
    }
}

// =============================================================================
// Clone
// =============================================================================

impl<Types: SwarmBootnodeTypes, C: Clone, S> Clone for Client<Types, C, S> {
    fn clone(&self) -> Self {
        Self {
            topology: Arc::clone(&self.topology),
            components: self.components.clone(),
            client_handle: self.client_handle.clone(),
            _storage: std::marker::PhantomData,
        }
    }
}

// =============================================================================
// SwarmClient trait implementation
// =============================================================================

#[async_trait]
impl<Types, A, P, S> SwarmClient
    for Client<Types, BuiltSwarmComponents<Types::Topology, A, P>, S>
where
    Types: SwarmClientTypes + 'static,
    A: Send + Sync + 'static,
    P: Send + Sync + 'static,
    S: Send + Sync + 'static,
{
    type Storage = S;

    async fn get(&self, address: &ChunkAddress) -> SwarmResult<AnyChunk> {
        let _closest = SwarmTopology::closest_to(&*self.topology, address, 3);
        let _handle = &self.client_handle;
        let _pricer = &self.components.pricer;
        let _accounting = &self.components.accounting;

        // TODO: Retrieval implementation
        Err(SwarmError::ChunkNotFound { address: *address })
    }

    async fn put(&self, chunk: AnyChunk, _storage: &Self::Storage) -> SwarmResult<()> {
        let _closest = SwarmTopology::closest_to(&*self.topology, &chunk.address(), 3);
        let _handle = &self.client_handle;
        let _pricer = &self.components.pricer;

        // TODO: Push sync implementation
        Ok(())
    }
}

// =============================================================================
// Type aliases for convenience
// =============================================================================

/// Bootnode client (topology only).
pub type BootnodeClient<Types> = Client<Types, (), ()>;

/// Client/Storer node (can retrieve and publish chunks).
pub type FullClient<Types, A, P, S> =
    Client<Types, BuiltSwarmComponents<<Types as SwarmBootnodeTypes>::Topology, A, P>, S>;

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use core::fmt::Debug;
    use tokio::sync::mpsc;
    use crate::{Accounting, ClientAccounting, FixedPricer, ClientCommand, ClientHandle, ClientService};
    use vertex_swarm_primitives::OverlayAddress;
    use vertex_swarm_bandwidth::DefaultAccountingConfig;
    use vertex_swarm_api::{SwarmBandwidthAccounting, SwarmNodeType, SwarmTopology};
    use vertex_tasks::SpawnableTask;
    use vertex_swarm_identity::Identity;

    struct MockNode;

    impl SpawnableTask for MockNode {
        fn into_task(self) -> impl std::future::Future<Output = ()> + Send {
            async {}
        }
    }

    #[derive(Clone)]
    struct MockTopology {
        identity: Identity,
    }

    impl Default for MockTopology {
        fn default() -> Self {
            Self {
                identity: Identity::random(vertex_swarmspec::init_testnet(), SwarmNodeType::Client),
            }
        }
    }

    impl SwarmTopology for MockTopology {
        type Identity = Identity;

        fn identity(&self) -> &Self::Identity {
            &self.identity
        }
        fn neighbors(&self, _depth: u8) -> Vec<OverlayAddress> {
            Vec::new()
        }
        fn depth(&self) -> u8 {
            0
        }
        fn closest_to(&self, _address: &ChunkAddress, _count: usize) -> Vec<OverlayAddress> {
            Vec::new()
        }
        fn add_peers(&self, _peers: &[OverlayAddress]) {}
        fn pick(&self, _peer: &OverlayAddress, _is_full_node: bool) -> bool {
            true
        }
        fn connected(&self, _peer: OverlayAddress) {}
        fn disconnected(&self, _peer: &OverlayAddress) {}
        fn peers_to_connect(&self) -> Vec<OverlayAddress> {
            Vec::new()
        }
    }

    #[derive(Clone, Debug)]
    struct MockSwarmBootnodeTypes;

    impl SwarmBootnodeTypes for MockSwarmBootnodeTypes {
        type Spec = vertex_swarmspec::Hive;
        type Identity = Identity;
        type Topology = MockTopology;
    }

    #[derive(Clone, Debug)]
    struct MockSwarmClientTypes;

    impl SwarmBootnodeTypes for MockSwarmClientTypes {
        type Spec = vertex_swarmspec::Hive;
        type Identity = Identity;
        type Topology = MockTopology;
    }

    impl SwarmClientTypes for MockSwarmClientTypes {
        type Accounting = ClientAccounting<Arc<Accounting<DefaultAccountingConfig, Identity>>, FixedPricer>;
    }

    fn test_identity() -> Identity {
        Identity::random(vertex_swarmspec::init_testnet(), SwarmNodeType::Client)
    }

    fn create_test_handle() -> ClientHandle {
        let (tx, _rx) = mpsc::unbounded_channel::<ClientCommand>();
        ClientHandle::new(tx)
    }

    #[test]
    fn test_bootnode_client() {
        let topology = MockTopology::default();
        let handle = create_test_handle();

        let client = Client::<MockSwarmBootnodeTypes>::bootnode(topology.clone(), handle.clone());
        let _ = client.topology().neighbors(0);

        // Type alias
        let _client: BootnodeClient<MockSwarmBootnodeTypes> = Client::bootnode(topology, handle);
    }

    #[test]
    fn test_bootnode_client_clone() {
        let topology = MockTopology::default();
        let handle = create_test_handle();
        let client = Client::<MockSwarmBootnodeTypes>::bootnode(topology, handle);
        let _clone = client.clone();
    }

    #[test]
    fn test_full_client() {
        let topology = MockTopology::default();
        let accounting = Accounting::new(DefaultAccountingConfig, test_identity());
        let pricer = FixedPricer::new(10_000, &*vertex_swarmspec::init_mainnet());
        let handle = create_test_handle();

        let client: FullClient<MockSwarmClientTypes, Arc<Accounting<DefaultAccountingConfig, Identity>>, FixedPricer, ()> =
            Client::full(topology.clone(), accounting, pricer.clone(), handle.clone());
        // full() wraps accounting in Arc internally
        let peers = SwarmBandwidthAccounting::peers(client.accounting().as_ref());
        assert!(peers.is_empty());

        // Type alias - accounting is wrapped in Arc by full()
        let accounting2 = Accounting::new(DefaultAccountingConfig, test_identity());
        let _client: FullClient<MockSwarmClientTypes, Arc<Accounting<DefaultAccountingConfig, Identity>>, FixedPricer, ()> =
            Client::full(topology, accounting2, pricer, handle);
    }

    #[test]
    fn test_full_client_clone() {
        let topology = MockTopology::default();
        let accounting = Accounting::new(DefaultAccountingConfig, test_identity());
        let pricer = FixedPricer::new(10_000, &*vertex_swarmspec::init_mainnet());
        let handle = create_test_handle();

        let client: FullClient<MockSwarmClientTypes, Arc<Accounting<DefaultAccountingConfig, Identity>>, FixedPricer, ()> =
            Client::full(topology, accounting, pricer, handle);
        let _clone = client.clone();
    }
}
