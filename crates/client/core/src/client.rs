//! SwarmClient implementation.
//!
//! Provides a unified client for all node types:
//! - Bootnodes: topology only
//! - Light nodes: + accounting + pricer, implements [`SwarmReader`]
//! - Publisher nodes: + [`SwarmWriter`] with storage proofs

use std::sync::Arc;

use async_trait::async_trait;
use vertex_primitives::{AnyChunk, ChunkAddress};
use vertex_swarm_api::{
    BootnodeTypes, LightTypes, SwarmError, SwarmReader, SwarmResult, SwarmWriter, Topology,
};
use vertex_swarm_builder::BuiltSwarmComponents;
use vertex_swarm_core::ClientHandle;

// =============================================================================
// SwarmClient
// =============================================================================

/// Unified Swarm client for all node types.
///
/// # Type Parameters
///
/// - `Types`: Node capability level ([`BootnodeTypes`], [`LightTypes`], etc.)
/// - `C`: Components - `()` for bootnodes, `BuiltSwarmComponents<T, A, P>` for light/publisher
/// - `S`: Storage proof type - `()` by default, postage stamp for publishers
///
/// # Examples
///
/// ```ignore
/// // Bootnode (peer discovery only)
/// let client = SwarmClient::<MyTypes>::bootnode(topology, handle);
///
/// // Light node (can retrieve chunks)
/// let client = SwarmClient::light(topology, accounting, pricer, handle);
/// let chunk = client.get(&address).await?;
///
/// // Publisher node (can also upload with storage proofs)
/// let client: SwarmClient<_, _, _, PostageStamp> = SwarmClient::publisher(topology, accounting, pricer, handle);
/// client.put(chunk, &stamp).await?;
/// ```
pub struct SwarmClient<Types: BootnodeTypes, C = (), S = ()> {
    topology: Arc<Types::Topology>,
    components: C,
    client_handle: ClientHandle,
    _storage: std::marker::PhantomData<S>,
}

// =============================================================================
// Bootnode (no components)
// =============================================================================

impl<Types: BootnodeTypes> SwarmClient<Types, (), ()> {
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
// Light node (with components, no storage proof type)
// =============================================================================

impl<Types, A, P> SwarmClient<Types, BuiltSwarmComponents<Types::Topology, A, P>, ()>
where
    Types: LightTypes,
    Types::Topology: Clone,
{
    /// Create a light client from pre-built components.
    ///
    /// Use [`Self::light`] for convenience when passing raw accounting/pricer.
    pub fn light_from_components(
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

impl<Types, A, P> SwarmClient<Types, BuiltSwarmComponents<Types::Topology, Arc<A>, P>, ()>
where
    Types: LightTypes,
    Types::Topology: Clone,
{
    /// Create a light client (can retrieve, uses `()` for storage).
    ///
    /// Wraps `accounting` in `Arc` for cheap cloning.
    pub fn light(
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
// Publisher node (with components and storage proof type)
// =============================================================================

impl<Types, A, P, S> SwarmClient<Types, BuiltSwarmComponents<Types::Topology, A, P>, S>
where
    Types: LightTypes,
    Types::Topology: Clone,
    S: Send + Sync + 'static,
{
    /// Create a publisher client from pre-built components.
    ///
    /// Use [`Self::publisher`] for convenience when passing raw accounting/pricer.
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

impl<Types, A, P, S> SwarmClient<Types, BuiltSwarmComponents<Types::Topology, Arc<A>, P>, S>
where
    Types: LightTypes,
    Types::Topology: Clone,
    S: Send + Sync + 'static,
{
    /// Create a publisher client (can retrieve and publish).
    ///
    /// Wraps `accounting` in `Arc` for cheap cloning.
    pub fn publisher(
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

impl<Types, A, P, S> SwarmClient<Types, BuiltSwarmComponents<Types::Topology, A, P>, S>
where
    Types: LightTypes,
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

impl<Types: BootnodeTypes, C, S> SwarmClient<Types, C, S> {
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

impl<Types: BootnodeTypes, C: Clone, S> Clone for SwarmClient<Types, C, S> {
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
// SwarmReader (for light/publisher nodes)
// =============================================================================

#[async_trait]
impl<Types, A, P, S> SwarmReader
    for SwarmClient<Types, BuiltSwarmComponents<Types::Topology, A, P>, S>
where
    Types: LightTypes + 'static,
    A: Send + Sync + 'static,
    P: Send + Sync + 'static,
    S: Send + Sync + 'static,
{
    async fn get(&self, address: &ChunkAddress) -> SwarmResult<AnyChunk> {
        let _closest = Topology::closest_to(&*self.topology, address, 3);
        let _handle = &self.client_handle;
        let _pricer = &self.components.pricer;
        let _accounting = &self.components.accounting;

        // TODO: Retrieval implementation
        Err(SwarmError::ChunkNotFound { address: *address })
    }
}

// =============================================================================
// SwarmWriter (for publisher nodes with storage proofs)
// =============================================================================

#[async_trait]
impl<Types, A, P, S> SwarmWriter
    for SwarmClient<Types, BuiltSwarmComponents<Types::Topology, A, P>, S>
where
    Types: LightTypes + 'static,
    A: Send + Sync + 'static,
    P: Send + Sync + 'static,
    S: Send + Sync + 'static,
{
    type Storage = S;

    async fn put(&self, chunk: AnyChunk, _storage: &Self::Storage) -> SwarmResult<()> {
        let _closest = Topology::closest_to(&*self.topology, &chunk.address(), 3);
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
pub type BootnodeClient<Types> = SwarmClient<Types, (), ()>;

/// Light client (can retrieve chunks).
pub type LightClient<Types, A, P> =
    SwarmClient<Types, BuiltSwarmComponents<<Types as BootnodeTypes>::Topology, A, P>, ()>;

/// Publisher client (can retrieve and publish chunks).
pub type PublisherClient<Types, A, P, S> =
    SwarmClient<Types, BuiltSwarmComponents<<Types as BootnodeTypes>::Topology, A, P>, S>;

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::B256;
    use core::fmt::Debug;
    use tokio::sync::mpsc;
    use vertex_bandwidth_core::{Accounting, AccountingConfig, FixedPricer};
    use vertex_primitives::OverlayAddress;
    use vertex_swarm_api::{AvailabilityAccounting, Identity};
    use vertex_swarm_core::ClientCommand;

    #[derive(Clone, Default)]
    struct MockTopology {
        self_addr: OverlayAddress,
    }

    impl Topology for MockTopology {
        fn self_address(&self) -> OverlayAddress {
            self.self_addr
        }
        fn neighbors(&self, _depth: u8) -> Vec<OverlayAddress> {
            Vec::new()
        }
        fn is_responsible_for(&self, _address: &ChunkAddress) -> bool {
            false
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
    struct MockIdentity;

    impl Identity for MockIdentity {
        type Spec = vertex_swarmspec::Hive;
        type Signer = alloy_signer_local::PrivateKeySigner;
        fn spec(&self) -> &Self::Spec {
            unimplemented!()
        }
        fn nonce(&self) -> B256 {
            B256::ZERO
        }
        fn signer(&self) -> Arc<Self::Signer> {
            unimplemented!()
        }
        fn is_full_node(&self) -> bool {
            false
        }
    }

    #[derive(Clone, Debug)]
    struct MockBootnodeTypes;

    impl BootnodeTypes for MockBootnodeTypes {
        type Spec = vertex_swarmspec::Hive;
        type Identity = MockIdentity;
        type Topology = MockTopology;
    }

    #[derive(Clone, Debug)]
    struct MockLightTypes;

    impl BootnodeTypes for MockLightTypes {
        type Spec = vertex_swarmspec::Hive;
        type Identity = MockIdentity;
        type Topology = MockTopology;
    }

    impl LightTypes for MockLightTypes {
        type Accounting = Accounting;
    }

    fn create_test_handle() -> ClientHandle {
        let (tx, _rx) = mpsc::unbounded_channel::<ClientCommand>();
        ClientHandle::new(tx)
    }

    #[test]
    fn test_bootnode_client() {
        let topology = MockTopology::default();
        let handle = create_test_handle();

        let client = SwarmClient::<MockBootnodeTypes>::bootnode(topology.clone(), handle.clone());
        let _ = client.topology().neighbors(0);

        // Type alias
        let _client: BootnodeClient<MockBootnodeTypes> = SwarmClient::bootnode(topology, handle);
    }

    #[test]
    fn test_bootnode_client_clone() {
        let topology = MockTopology::default();
        let handle = create_test_handle();
        let client = SwarmClient::<MockBootnodeTypes>::bootnode(topology, handle);
        let _clone = client.clone();
    }

    #[test]
    fn test_light_client() {
        let topology = MockTopology::default();
        let accounting = Accounting::new(AccountingConfig::default());
        let pricer = FixedPricer::default();
        let handle = create_test_handle();

        let client = SwarmClient::<MockLightTypes, _, _>::light(
            topology.clone(),
            accounting,
            pricer.clone(),
            handle.clone(),
        );
        let peers = client.accounting().peers();
        assert!(peers.is_empty());

        // Type alias - accounting is wrapped in Arc by light()
        let accounting2 = Accounting::new(AccountingConfig::default());
        let _client: LightClient<MockLightTypes, Arc<Accounting>, FixedPricer> =
            SwarmClient::light(topology, accounting2, pricer, handle);
    }

    #[test]
    fn test_light_client_clone() {
        let topology = MockTopology::default();
        let accounting = Accounting::new(AccountingConfig::default());
        let pricer = FixedPricer::default();
        let handle = create_test_handle();

        let client =
            SwarmClient::<MockLightTypes, _, _>::light(topology, accounting, pricer, handle);
        let _clone = client.clone();
    }

    #[test]
    fn test_publisher_client() {
        let topology = MockTopology::default();
        let accounting = Accounting::new(AccountingConfig::default());
        let pricer = FixedPricer::default();
        let handle = create_test_handle();

        // Publisher with unit storage - accounting is wrapped in Arc by publisher()
        let client: PublisherClient<MockLightTypes, Arc<Accounting>, FixedPricer, ()> =
            SwarmClient::publisher(topology, accounting, pricer, handle);

        let peers = client.accounting().peers();
        assert!(peers.is_empty());
    }
}
