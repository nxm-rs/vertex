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
//! let builder = SwarmNodeBuilder::<node_type::Client>::new(&ctx, &args);
//!
//! // Or customize components
//! let builder = SwarmNodeBuilder::<node_type::Client>::new(&ctx, &args)
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
use tokio::sync::mpsc;
use vertex_swarm_bandwidth::{Accounting, ClientAccounting, DefaultAccountingConfig, DefaultPricingConfig, FixedPricer};
use vertex_swarm_bandwidth_pseudosettle::{PseudosettleProvider, create_pseudosettle_actor};
use vertex_swarm_peermanager::PeerStore;
use vertex_node_api::{NodeBuildsProtocol, NodeContext};
use vertex_swarm_api::{
    SwarmAccountingConfig, SwarmLaunchConfig, SwarmNetworkConfig, SwarmPricingConfig,
    SwarmProtocol, Services,
};
use vertex_swarm_core::{ClientCommand, SwarmNode};
use vertex_swarm_core::args::SwarmArgs;
use vertex_swarm_identity::Identity;
use vertex_swarmspec::Hive;
use vertex_tasks::TaskExecutor;

use crate::error::SwarmNodeError;
use crate::launch::SwarmLaunchContext;
use crate::node_type::{Bootnode, Client, NodeTypeDefaults, Storer};
use crate::types::{DefaultClientTypes, DefaultNetworkConfig};

/// Generic Swarm node builder parameterized by node type and component builders.
///
/// The node type `N` determines default component builders via [`NodeTypeDefaults`].
/// Component builders can be overridden using the builder methods.
///
/// # Type Parameters
///
/// - `N`: Node type marker (e.g., `Client`, `Storer`, `Bootnode`)
/// - `TB`: Topology builder type
/// - `AB`: Accounting builder type
/// - `PB`: Pricer builder type
pub struct SwarmNodeBuilder<
    N: NodeTypeDefaults,
    TB = <N as NodeTypeDefaults>::DefaultTopology,
    AB = <N as NodeTypeDefaults>::DefaultAccounting,
    PB = <N as NodeTypeDefaults>::DefaultPricer,
> {
    identity: Arc<Identity>,
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
            nat_addrs: ctx.config.protocol.network.nat_addrs(),
            nat_auto: ctx.config.protocol.network.nat_auto_enabled(),
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
/// This implements `NodeBuildsProtocol` and can be passed to `NodeBuilder::with_protocol`.
pub struct ClientNodeBuildConfig {
    identity: Arc<Identity>,
    spec: Arc<Hive>,
    peer_store: Arc<dyn PeerStore>,
    peers_path: std::path::PathBuf,
    network_config: DefaultNetworkConfig,
}

impl<TB, AB, PB> SwarmNodeBuilder<Client, TB, AB, PB> {
    /// Build the light node configuration.
    ///
    /// Returns a `ClientNodeBuildConfig` that implements `NodeBuildsProtocol`.
    pub fn build(self) -> ClientNodeBuildConfig {
        ClientNodeBuildConfig {
            identity: self.identity,
            spec: self.spec,
            peer_store: self.peer_store,
            peers_path: self.peers_path,
            network_config: self.network_config,
        }
    }
}

impl NodeBuildsProtocol for ClientNodeBuildConfig {
    type Protocol = SwarmProtocol<Self>;

    fn protocol_name(&self) -> &'static str {
        "Swarm"
    }
}

#[async_trait]
impl SwarmLaunchConfig for ClientNodeBuildConfig {
    type Types = DefaultClientTypes;
    type Components = crate::rpc::ClientNodeRpcComponents;
    type Error = SwarmNodeError;

    async fn build(
        self,
        _ctx: &NodeContext,
    ) -> Result<(Self::Components, Services<Self::Types>), Self::Error> {
        use tracing::info;
        use vertex_swarm_api::ClientComponents;
        use vertex_swarmspec::Loggable;

        info!("Building {} node...", Client::NAME);
        self.spec.log();
        self.identity.log();
        info!("Peers database: {}", self.peers_path.display());

        // Create event channels for settlement services
        let (pseudosettle_event_tx, pseudosettle_event_rx) = mpsc::unbounded_channel();

        // Build the SwarmNode with event routing configured
        let (node, client_service, client_handle) =
            SwarmNode::<DefaultClientTypes>::builder(self.identity.clone())
                .with_network_config(&self.network_config)
                .with_peer_store(self.peer_store)
                .with_pseudosettle_events(pseudosettle_event_tx)
                .build()
                .await
                .map_err(|e| SwarmNodeError::Build(e.to_string()))?;

        // Get the topology from the node
        let topology = node.kademlia_topology().clone();

        // Create accounting configuration
        let config = DefaultAccountingConfig;

        // Create a command channel sender for settlement services
        // (Settlement services send commands via this channel)
        let (settlement_command_tx, mut settlement_command_rx) =
            mpsc::unbounded_channel::<ClientCommand>();

        // Create accounting first (services will use it)
        let accounting = Arc::new(Accounting::with_providers(
            config.clone(),
            self.identity.clone(),
            // Start with empty providers - we'll add the handle-backed provider
            vec![],
        ));

        // Create pseudosettle actor with the accounting reference
        let (pseudosettle_service, pseudosettle_handle) = create_pseudosettle_actor(
            pseudosettle_event_rx,
            settlement_command_tx.clone(),
            accounting.clone(),
            config.refresh_rate(),
        );

        // Create bandwidth accounting with the handle-backed provider
        let bandwidth = Arc::new(Accounting::with_providers(
            config.clone(),
            self.identity.clone(),
            vec![Box::new(PseudosettleProvider::with_handle(
                config,
                pseudosettle_handle,
            ))],
        ));

        let providers = bandwidth.provider_names();
        if providers.is_empty() {
            info!("Bandwidth incentives: disabled");
        } else {
            info!("Bandwidth incentives: {}", providers.join(", "));
        }

        // Create pricing strategy
        let pricing_config = DefaultPricingConfig;
        let pricing = FixedPricer::new(pricing_config.base_price(), self.spec.as_ref());

        // Combine bandwidth accounting and pricing
        let accounting = ClientAccounting::new(bandwidth, pricing);

        // Spawn settlement services
        let executor = TaskExecutor::current();
        executor.spawn(pseudosettle_service.into_task());

        // Spawn a task to forward settlement commands to the client handle
        let client_handle_for_settlement = client_handle.clone();
        executor.spawn(async move {
            while let Some(cmd) = settlement_command_rx.recv().await {
                if let Err(e) = client_handle_for_settlement.send_command(cmd) {
                    tracing::warn!(error = %e, "Failed to forward settlement command to client");
                }
            }
        });

        // Clone client_handle before moving into services
        let client_handle_for_components = client_handle.clone();

        // Create components (including client_handle for RPC), wrapped for RPC registration
        let components = crate::rpc::ClientNodeRpcComponents(ClientComponents::new(
            self.identity,
            topology,
            accounting,
            client_handle_for_components,
        ));

        // Services implement SpawnableTask directly - no wrappers needed
        let services = Services::new(node, client_service, client_handle);

        info!("{} node built successfully", Client::NAME);
        Ok((components, services))
    }
}

impl<TB, AB, PB> SwarmNodeBuilder<Storer, TB, AB, PB> {
    /// Build the full node configuration.
    pub fn build(self) -> ! {
        unimplemented!("Storer node builder not yet implemented")
    }
}

impl<TB, AB, PB> SwarmNodeBuilder<Bootnode, TB, AB, PB> {
    /// Build the bootnode configuration.
    pub fn build(self) -> BootnodeBuildConfig {
        BootnodeBuildConfig {
            identity: self.identity,
            spec: self.spec,
            peer_store: self.peer_store,
            peers_path: self.peers_path,
            network_config: self.network_config,
        }
    }
}

/// Build config for bootnodes.
pub struct BootnodeBuildConfig {
    identity: Arc<Identity>,
    spec: Arc<Hive>,
    peer_store: Arc<dyn PeerStore>,
    peers_path: std::path::PathBuf,
    network_config: DefaultNetworkConfig,
}

impl NodeBuildsProtocol for BootnodeBuildConfig {
    type Protocol = SwarmProtocol<Self>;

    fn protocol_name(&self) -> &'static str {
        "Swarm (Bootnode)"
    }
}

#[async_trait]
impl SwarmLaunchConfig for BootnodeBuildConfig {
    type Types = crate::types::DefaultBootnodeTypes;
    type Components = crate::rpc::BootnodeRpcComponents;
    type Error = SwarmNodeError;

    async fn build(
        self,
        _ctx: &NodeContext,
    ) -> Result<(Self::Components, Services<Self::Types>), Self::Error> {
        use tracing::info;
        use vertex_swarmspec::Loggable;

        info!("Building {} node...", Bootnode::NAME);
        self.spec.log();
        self.identity.log();
        info!("Peers database: {}", self.peers_path.display());

        // Build the BootNode (no client service/handle)
        let node = vertex_swarm_core::BootNode::<crate::types::DefaultBootnodeTypes>::builder(
            self.identity.clone(),
        )
        .with_network_config(&self.network_config)
        .with_peer_store(self.peer_store)
        .build()
        .await
        .map_err(|e| SwarmNodeError::Build(e.to_string()))?;

        // Get topology for components
        let topology = node.kademlia_topology().clone();

        // Create components
        let components = crate::rpc::BootnodeRpcComponents {
            identity: self.identity,
            topology,
        };

        // Create services with no-op client service/handle
        let services = Services::new(
            node,
            crate::types::NoOpClientService,
            crate::types::NoOpClientHandle,
        );

        info!("{} node built successfully", Bootnode::NAME);
        Ok((components, services))
    }
}
