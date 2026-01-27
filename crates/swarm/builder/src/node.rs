//! Generic Swarm node builder for protocol configuration.
//!
//! This module provides `SwarmNodeBuilder<N, TB, AB, PB>` - a generic builder
//! that configures Swarm protocol components based on node type.
//!
//! # Usage
//!
//! ```ignore
//! use vertex_swarm_builder::{SwarmNodeBuilder, node_type};
//!
//! // Create a light node builder with defaults
//! let builder = SwarmNodeBuilder::<node_type::Light>::new(&ctx, &args);
//!
//! // Or customize components
//! let builder = SwarmNodeBuilder::<node_type::Light>::new(&ctx, &args)
//!     .accounting(CustomAccountingBuilder::new());
//!
//! // Use with NodeBuilder
//! NodeBuilder::new()
//!     .with_context(&ctx, &args.infra)
//!     .with_protocol(builder)
//!     .launch()
//!     .await?;
//! ```

use std::marker::PhantomData;
use std::sync::Arc;

use async_trait::async_trait;
use vertex_bandwidth_core::Accounting;
use vertex_client_peermanager::PeerStore;
use vertex_node_api::{BuildsProtocol, NodeContext};
use vertex_swarm_api::{
    Identity, NetworkConfig, SwarmBuildConfig, SwarmLightComponents, SwarmProtocol, SwarmServices,
};
use vertex_swarm_core::SwarmNode;
use vertex_swarm_core::args::SwarmArgs;
use vertex_swarm_identity::SwarmIdentity;
use vertex_swarmspec::Hive;

use crate::error::SwarmNodeError;
use crate::launch::SwarmLaunchContext;
use crate::node_type::{Bootnode, Full, Light, NodeTypeDefaults, Publisher, Staker};
use crate::types::{ClientServiceRunner, DefaultLightTypes, DefaultNetworkConfig, SwarmNodeRunner};

/// Generic Swarm node builder parameterized by node type and component builders.
///
/// The node type `N` determines default component builders via [`NodeTypeDefaults`].
/// Component builders can be overridden using the builder methods.
///
/// # Type Parameters
///
/// - `N`: Node type marker (e.g., `Light`, `Full`, `Bootnode`)
/// - `TB`: Topology builder type
/// - `AB`: Accounting builder type
/// - `PB`: Pricer builder type
pub struct SwarmNodeBuilder<
    N: NodeTypeDefaults,
    TB = <N as NodeTypeDefaults>::DefaultTopology,
    AB = <N as NodeTypeDefaults>::DefaultAccounting,
    PB = <N as NodeTypeDefaults>::DefaultPricer,
> {
    identity: Arc<SwarmIdentity>,
    spec: Arc<Hive>,
    peer_store: Arc<dyn PeerStore>,
    peers_path: std::path::PathBuf,
    network_config: DefaultNetworkConfig,
    topology_builder: TB,
    accounting_builder: AB,
    pricer_builder: PB,
    _node_type: PhantomData<N>,
}

impl<N: NodeTypeDefaults> SwarmNodeBuilder<N> {
    /// Create a new builder with default components for the node type.
    ///
    /// Extracts configuration from the launch context and swarm args.
    pub fn new(ctx: &SwarmLaunchContext, _args: &SwarmArgs) -> Self {
        let network_config = DefaultNetworkConfig {
            listen_addrs: ctx.config.protocol.network.listen_addrs(),
            bootnodes: ctx.config.protocol.network.bootnodes(),
            discovery_enabled: ctx.config.protocol.network.discovery_enabled(),
            max_peers: ctx.config.protocol.network.max_peers(),
            idle_timeout_secs: ctx.config.protocol.network.idle_timeout().as_secs(),
        };

        SwarmNodeBuilder {
            identity: Arc::new(ctx.identity.clone()),
            spec: ctx.spec.clone(),
            peer_store: ctx.peer_store.clone(),
            peers_path: ctx.peers_path.clone(),
            network_config,
            topology_builder: N::DefaultTopology::default(),
            accounting_builder: N::DefaultAccounting::default(),
            pricer_builder: N::DefaultPricer::default(),
            _node_type: PhantomData,
        }
    }
}

impl<N: NodeTypeDefaults, TB, AB, PB> SwarmNodeBuilder<N, TB, AB, PB> {
    /// Override the topology builder.
    pub fn topology<NewTB>(self, builder: NewTB) -> SwarmNodeBuilder<N, NewTB, AB, PB> {
        SwarmNodeBuilder {
            identity: self.identity,
            spec: self.spec,
            peer_store: self.peer_store,
            peers_path: self.peers_path,
            network_config: self.network_config,
            topology_builder: builder,
            accounting_builder: self.accounting_builder,
            pricer_builder: self.pricer_builder,
            _node_type: PhantomData,
        }
    }

    /// Override the accounting builder.
    pub fn accounting<NewAB>(self, builder: NewAB) -> SwarmNodeBuilder<N, TB, NewAB, PB> {
        SwarmNodeBuilder {
            identity: self.identity,
            spec: self.spec,
            peer_store: self.peer_store,
            peers_path: self.peers_path,
            network_config: self.network_config,
            topology_builder: self.topology_builder,
            accounting_builder: builder,
            pricer_builder: self.pricer_builder,
            _node_type: PhantomData,
        }
    }

    /// Override the pricer builder.
    pub fn pricer<NewPB>(self, builder: NewPB) -> SwarmNodeBuilder<N, TB, AB, NewPB> {
        SwarmNodeBuilder {
            identity: self.identity,
            spec: self.spec,
            peer_store: self.peer_store,
            peers_path: self.peers_path,
            network_config: self.network_config,
            topology_builder: self.topology_builder,
            accounting_builder: self.accounting_builder,
            pricer_builder: builder,
            _node_type: PhantomData,
        }
    }

    /// Override network configuration.
    pub fn network_config(mut self, config: DefaultNetworkConfig) -> Self {
        self.network_config = config;
        self
    }
}

/// Build config for light nodes produced by SwarmNodeBuilder.
///
/// This implements `BuildsProtocol` and can be passed to `NodeBuilder::with_protocol`.
pub struct LightNodeBuildConfig {
    identity: Arc<SwarmIdentity>,
    spec: Arc<Hive>,
    peer_store: Arc<dyn PeerStore>,
    peers_path: std::path::PathBuf,
    network_config: DefaultNetworkConfig,
}

impl<TB, AB, PB> SwarmNodeBuilder<Light, TB, AB, PB> {
    /// Build the light node configuration.
    ///
    /// Returns a `LightNodeBuildConfig` that implements `BuildsProtocol`.
    pub fn build(self) -> LightNodeBuildConfig {
        LightNodeBuildConfig {
            identity: self.identity,
            spec: self.spec,
            peer_store: self.peer_store,
            peers_path: self.peers_path,
            network_config: self.network_config,
        }
    }
}

impl BuildsProtocol for LightNodeBuildConfig {
    type Protocol = SwarmProtocol<Self>;

    fn protocol_name(&self) -> &'static str {
        "Swarm"
    }
}

#[async_trait]
impl SwarmBuildConfig for LightNodeBuildConfig {
    type Types = DefaultLightTypes;
    type Components = SwarmLightComponents<DefaultLightTypes>;
    type Error = SwarmNodeError;

    async fn build(
        self,
        _ctx: &NodeContext,
    ) -> Result<(Self::Components, SwarmServices<Self::Types>), Self::Error> {
        use tracing::info;
        use vertex_swarmspec::SwarmSpec;

        info!("Node type: Light");
        info!(
            "Network: {} (ID: {})",
            self.spec.network_name(),
            self.spec.network_id()
        );
        info!("Overlay address: {}", self.identity.overlay_address());
        info!("Ethereum address: {}", self.identity.signer().address());
        info!("Peers database: {}", self.peers_path.display());
        info!("");
        info!("Starting Swarm Light node...");

        // Build the SwarmNode which creates the topology and client service
        let (node, client_service, client_handle) =
            SwarmNode::<DefaultLightTypes>::builder(self.identity.clone())
                .with_network_config(&self.network_config)
                .with_peer_store(self.peer_store)
                .build()
                .await
                .map_err(|e| SwarmNodeError::Build(e.to_string()))?;

        // Get the topology from the node
        let topology = node.kademlia_topology().clone();

        // Create accounting
        let accounting = Arc::new(Accounting::new(
            vertex_bandwidth_core::AccountingConfig::default(),
        ));

        // Create components
        let components = SwarmLightComponents::new(self.identity, topology, accounting);

        // Wrap services in runners that implement the traits
        let node_runner = SwarmNodeRunner::new(node);
        let service_runner = ClientServiceRunner::new(client_service);

        let services = SwarmServices::new(node_runner, service_runner, client_handle);

        info!("Light node built successfully");
        Ok((components, services))
    }
}

impl<TB, AB, PB> SwarmNodeBuilder<Full, TB, AB, PB> {
    /// Build the full node configuration.
    pub fn build(self) -> ! {
        unimplemented!("Full node builder not yet implemented")
    }
}

impl<TB, AB, PB> SwarmNodeBuilder<Publisher, TB, AB, PB> {
    /// Build the publisher node configuration.
    pub fn build(self) -> ! {
        unimplemented!("Publisher node builder not yet implemented")
    }
}

impl<TB, AB, PB> SwarmNodeBuilder<Bootnode, TB, AB, PB> {
    /// Build the bootnode configuration.
    pub fn build(self) -> ! {
        unimplemented!("Bootnode builder not yet implemented")
    }
}

impl<TB, AB, PB> SwarmNodeBuilder<Staker, TB, AB, PB> {
    /// Build the staker node configuration.
    pub fn build(self) -> ! {
        unimplemented!("Staker node builder not yet implemented")
    }
}
