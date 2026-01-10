//! Node builder and configuration

use crate::{FullNodeComponents, NodeTypes};
use vertex_primitives::Result;
use vertex_swarm_api::{
    AccessController, BandwidthController, ChunkFactory, ChunkStore, NetworkClient,
    NetworkConfig, StorageConfig
};
use vertex_swarmspec::SwarmSpec;

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, string::String, vec::Vec};

/// A builder context for creating node components
#[derive(Clone)]
pub struct BuilderContext<N: NodeTypes> {
    /// Swarm specification
    pub spec: N::Spec,
    /// Network configuration
    pub network_config: NetworkConfig,
    /// Storage configuration
    pub storage_config: StorageConfig,
}

impl<N: NodeTypes> BuilderContext<N> {
    /// Create a new builder context
    pub fn new(
        spec: N::Spec,
        network_config: NetworkConfig,
        storage_config: StorageConfig,
    ) -> Self {
        Self {
            spec,
            network_config,
            storage_config,
        }
    }
}

/// A trait for building node components
#[async_trait]
#[auto_impl::auto_impl(&, Arc)]
pub trait NodeComponentsBuilder<N: NodeTypes>: Send + 'static {
    /// The components this builder creates
    type Components: FullNodeComponents<Types = N>;

    /// Build the node components
    async fn build_components(
        self,
        context: &BuilderContext<N>,
    ) -> Result<Self::Components>;
}

/// A generic components builder
pub struct ComponentsBuilder<N, CF, AC, SC, NC, BC> {
    /// Chunk factory builder
    chunk_factory_builder: CF,
    /// Access controller builder
    access_controller_builder: AC,
    /// Chunk store builder
    chunk_store_builder: SC,
    /// Network client builder
    network_client_builder: NC,
    /// Bandwidth controller builder
    bandwidth_controller_builder: BC,
    /// Node types marker
    _marker: core::marker::PhantomData<N>,
}

impl<N, CF, AC, SC, NC, BC> ComponentsBuilder<N, CF, AC, SC, NC, BC> {
    /// Create a new components builder
    pub fn new(
        chunk_factory_builder: CF,
        access_controller_builder: AC,
        chunk_store_builder: SC,
        network_client_builder: NC,
        bandwidth_controller_builder: BC,
    ) -> Self {
        Self {
            chunk_factory_builder,
            access_controller_builder,
            chunk_store_builder,
            network_client_builder,
            bandwidth_controller_builder,
            _marker: core::marker::PhantomData,
        }
    }
}

/// A trait for building chunk factories
#[async_trait]
#[auto_impl::auto_impl(&, Arc)]
pub trait ChunkFactoryBuilder<N: NodeTypes>: Send + 'static {
    /// Build a chunk factory
    async fn build_chunk_factory(
        &self,
        context: &BuilderContext<N>,
    ) -> Result<N::ChunkFactory>;
}

/// A trait for building access controllers
#[async_trait]
#[auto_impl::auto_impl(&, Arc)]
pub trait AccessControllerBuilder<N: NodeTypes>: Send + 'static {
    /// Build an access controller
    async fn build_access_controller(
        &self,
        context: &BuilderContext<N>,
    ) -> Result<N::AccessController>;
}

/// A trait for building chunk stores
#[async_trait]
#[auto_impl::auto_impl(&, Arc)]
pub trait ChunkStoreBuilder<N: NodeTypes>: Send + 'static {
    /// Build a chunk store
    async fn build_chunk_store(
        &self,
        context: &BuilderContext<N>,
    ) -> Result<N::ChunkStore>;
}

/// A trait for building network clients
#[async_trait]
#[auto_impl::auto_impl(&, Arc)]
pub trait NetworkClientBuilder<N: NodeTypes>: Send + 'static {
    /// Build a network client
    async fn build_network_client(
        &self,
        context: &BuilderContext<N>,
    ) -> Result<N::NetworkClient>;
}

/// A trait for building bandwidth controllers
#[async_trait]
#[auto_impl::auto_impl(&, Arc)]
pub trait BandwidthControllerBuilder<N: NodeTypes>: Send + 'static {
    /// Build a bandwidth controller
    async fn build_bandwidth_controller(
        &self,
        context: &BuilderContext<N>,
    ) -> Result<N::BandwidthController>;
}

#[async_trait]
impl<N, CF, AC, SC, NC, BC> NodeComponentsBuilder<N> for ComponentsBuilder<N, CF, AC, SC, NC, BC>
where
    N: NodeTypes,
    CF: ChunkFactoryBuilder<N> + Send + 'static,
    AC: AccessControllerBuilder<N> + Send + 'static,
    SC: ChunkStoreBuilder<N> + Send + 'static,
    NC: NetworkClientBuilder<N> + Send + 'static,
    BC: BandwidthControllerBuilder<N> + Send + 'static,
{
    type Components = crate::NodeComponents<N>;

    async fn build_components(
        self,
        context: &BuilderContext<N>,
    ) -> Result<Self::Components> {
        // Build all components in the appropriate order
        let chunk_factory = self.chunk_factory_builder.build_chunk_factory(context).await?;
        let access_controller = self.access_controller_builder.build_access_controller(context).await?;
        let chunk_store = self.chunk_store_builder.build_chunk_store(context).await?;
        let network_client = self.network_client_builder.build_network_client(context).await?;
        let bandwidth_controller = self.bandwidth_controller_builder.build_bandwidth_controller(context).await?;

        Ok(crate::NodeComponents {
            spec: context.spec.clone(),
            chunk_factory,
            access_controller,
            chunk_store,
            network_client,
            bandwidth_controller,
        })
    }
}
