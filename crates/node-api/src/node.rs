//! Node types and components

use crate::{swarm_api::*, swarmspec::SwarmSpec};
use core::{fmt::Debug, marker::PhantomData};
use vertex_primitives::{ChunkAddress, Result};

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, string::String, vec::Vec};

/// The type that provides the essential types of a Swarm node.
///
/// This includes the primitive types and components needed for node operation.
pub trait NodeTypes: Send + Sync + Unpin + 'static {
    /// The network specification.
    type Spec: SwarmSpec;

    /// The chunk factory used to create chunks.
    type ChunkFactory: ChunkFactory;

    /// The access controller for managing permissions.
    type AccessController: AccessController;

    /// The chunk store for persistence.
    type ChunkStore: ChunkStore;

    /// The network client for communication.
    type NetworkClient: NetworkClient;

    /// The bandwidth controller.
    type BandwidthController: BandwidthController;
}

/// A fully configured node with all components.
pub trait FullNodeComponents: Send + Sync + Clone + 'static {
    /// The node's types.
    type Types: NodeTypes;

    /// Returns the node's swarm specification.
    fn spec(&self) -> &<Self::Types as NodeTypes>::Spec;

    /// Returns the node's chunk factory.
    fn chunk_factory(&self) -> &<Self::Types as NodeTypes>::ChunkFactory;

    /// Returns the node's access controller.
    fn access_controller(&self) -> &<Self::Types as NodeTypes>::AccessController;

    /// Returns the node's chunk store.
    fn chunk_store(&self) -> &<Self::Types as NodeTypes>::ChunkStore;

    /// Returns the node's network client.
    fn network_client(&self) -> &<Self::Types as NodeTypes>::NetworkClient;

    /// Returns the node's bandwidth controller.
    fn bandwidth_controller(&self) -> &<Self::Types as NodeTypes>::BandwidthController;

    /// Creates a new chunk from data
    async fn create_chunk(&self, data: Vec<u8>) -> Result<Box<dyn Chunk>> {
        self.chunk_factory().create(data)
    }

    /// Store a chunk in the Swarm network
    async fn store_chunk(
        &self,
        chunk: Box<dyn Chunk>,
        credential: &dyn Credential,
    ) -> Result<()> {
        // Verify chunk can be stored
        self.access_controller().check_storage_permission(&*chunk, credential)?;

        // Store locally if we're responsible
        if self.is_responsible_for(chunk.address()) {
            self.chunk_store().put(chunk.clone_box(), credential)?;
        }

        // Store in network
        self.network_client().store(chunk, credential).await
    }

    /// Retrieve a chunk from the Swarm network
    async fn retrieve_chunk(
        &self,
        address: &ChunkAddress,
        credential: Option<&dyn Credential>,
    ) -> Result<Box<dyn Chunk>> {
        // Check if we have it locally
        if let Ok(Some(chunk)) = self.chunk_store().get(address) {
            // Record the retrieval
            self.access_controller().record_retrieval(address)?;
            return Ok(chunk);
        }

        // Retrieve from network
        let chunk = self.network_client().retrieve(address, credential).await?;

        // Store locally if we're responsible
        if self.is_responsible_for(address) {
            self.chunk_store().put(chunk.clone_box(), credential.unwrap_or_else(|| panic!("Credential required for storage")))?;
        }

        Ok(chunk)
    }

    /// Check if this node is responsible for a chunk
    fn is_responsible_for(&self, address: &ChunkAddress) -> bool {
        // Default implementation - override in concrete types
        false
    }
}

/// An adapter type that adds specific implementations to node types.
#[derive(Debug)]
pub struct NodeTypesAdapter<Spec, ChunkF, AccessC, Store, Network, Bandwidth> {
    /// Swarm specification
    spec: PhantomData<Spec>,
    /// Chunk factory
    chunk_factory: PhantomData<ChunkF>,
    /// Access controller
    access_controller: PhantomData<AccessC>,
    /// Chunk store
    chunk_store: PhantomData<Store>,
    /// Network client
    network_client: PhantomData<Network>,
    /// Bandwidth controller
    bandwidth_controller: PhantomData<Bandwidth>,
}

impl<Spec, ChunkF, AccessC, Store, Network, Bandwidth> Default
    for NodeTypesAdapter<Spec, ChunkF, AccessC, Store, Network, Bandwidth>
{
    fn default() -> Self {
        Self::new()
    }
}

impl<Spec, ChunkF, AccessC, Store, Network, Bandwidth>
    NodeTypesAdapter<Spec, ChunkF, AccessC, Store, Network, Bandwidth>
{
    /// Create a new adapter with the configured types.
    pub const fn new() -> Self {
        Self {
            spec: PhantomData,
            chunk_factory: PhantomData,
            access_controller: PhantomData,
            chunk_store: PhantomData,
            network_client: PhantomData,
            bandwidth_controller: PhantomData,
        }
    }
}

impl<Spec, ChunkF, AccessC, Store, Network, Bandwidth> NodeTypes
    for NodeTypesAdapter<Spec, ChunkF, AccessC, Store, Network, Bandwidth>
where
    Spec: SwarmSpec,
    ChunkF: ChunkFactory,
    AccessC: AccessController,
    Store: ChunkStore,
    Network: NetworkClient,
    Bandwidth: BandwidthController,
{
    type Spec = Spec;
    type ChunkFactory = ChunkF;
    type AccessController = AccessC;
    type ChunkStore = Store;
    type NetworkClient = Network;
    type BandwidthController = Bandwidth;
}

/// Container for actual node component instances.
#[derive(Clone)]
pub struct NodeComponents<T: NodeTypes> {
    /// Swarm specification
    pub spec: T::Spec,
    /// Chunk factory
    pub chunk_factory: T::ChunkFactory,
    /// Access controller
    pub access_controller: T::AccessController,
    /// Chunk store
    pub chunk_store: T::ChunkStore,
    /// Network client
    pub network_client: T::NetworkClient,
    /// Bandwidth controller
    pub bandwidth_controller: T::BandwidthController,
}

impl<T: NodeTypes> FullNodeComponents for NodeComponents<T> {
    type Types = T;

    fn spec(&self) -> &T::Spec {
        &self.spec
    }

    fn chunk_factory(&self) -> &T::ChunkFactory {
        &self.chunk_factory
    }

    fn access_controller(&self) -> &T::AccessController {
        &self.access_controller
    }

    fn chunk_store(&self) -> &T::ChunkStore {
        &self.chunk_store
    }

    fn network_client(&self) -> &T::NetworkClient {
        &self.network_client
    }

    fn bandwidth_controller(&self) -> &T::BandwidthController {
        &self.bandwidth_controller
    }

    fn is_responsible_for(&self, address: &ChunkAddress) -> bool {
        // Implementation based on neighborhood depth
        let node_address = self.network_client().status().neighborhood_depth;

        // TODO: Implement proper neighborhood calculation
        // This is a placeholder implementation
        true
    }
}
