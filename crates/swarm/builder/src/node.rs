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
use vertex_node_api::{NodeBuildsProtocol, NodeContext};
use vertex_swarm_api::{
    NodeTask, SwarmAccountingConfig, SwarmLaunchConfig, SwarmNetworkConfig, SwarmProtocol,
};
use vertex_swarm_bandwidth::{Accounting, DefaultAccountingConfig};
use vertex_swarm_bandwidth_pseudosettle::{PseudosettleProvider, create_pseudosettle_actor};
use vertex_swarm_identity::Identity;
use vertex_swarm_node::args::ProtocolArgs;
use vertex_swarm_node::{ClientCommand, SwarmNode};
use vertex_swarm_peermanager::PeerStore;
use vertex_swarmspec::Hive;
use vertex_tasks::{SpawnableTask, TaskExecutor};

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
    pub fn new(ctx: &SwarmLaunchContext, _args: &ProtocolArgs) -> Self {
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
    type Providers = crate::rpc::ClientRpcProviders<crate::providers::NetworkChunkProvider>;
    type Error = SwarmNodeError;

    async fn build(self, _ctx: &NodeContext) -> Result<(NodeTask, Self::Providers), Self::Error> {
        use tracing::info;
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
        let (settlement_command_tx, mut settlement_command_rx) =
            mpsc::unbounded_channel::<ClientCommand>();

        // Create accounting for pseudosettle
        let accounting = Arc::new(Accounting::with_providers(
            config,
            self.identity.clone(),
            vec![],
        ));

        // Create pseudosettle actor
        let (pseudosettle_service, pseudosettle_handle) = create_pseudosettle_actor(
            pseudosettle_event_rx,
            settlement_command_tx.clone(),
            accounting.clone(),
            config.refresh_rate(),
        );

        // Create bandwidth accounting with pseudosettle provider
        let bandwidth = Arc::new(Accounting::with_providers(
            config,
            self.identity.clone(),
            vec![Box::new(PseudosettleProvider::with_handle(
                config,
                pseudosettle_handle,
            ))],
        ));

        let provider_names = bandwidth.provider_names();
        if provider_names.is_empty() {
            info!("Bandwidth incentives: disabled");
        } else {
            info!("Bandwidth incentives: {}", provider_names.join(", "));
        }

        // Create chunk provider for RPC
        let chunk_provider =
            crate::providers::NetworkChunkProvider::new(client_handle.clone(), topology.clone());

        // Create RPC providers
        let providers = crate::rpc::ClientRpcProviders::new(topology, chunk_provider);

        // Create the main event loop task
        let task: NodeTask = Box::pin(async move {
            let executor = TaskExecutor::current();

            // Spawn settlement services
            executor.spawn(pseudosettle_service.into_task());

            // Forward settlement commands to client handle
            let client_handle_for_settlement = client_handle.clone();
            executor.spawn(async move {
                while let Some(cmd) = settlement_command_rx.recv().await {
                    if let Err(e) = client_handle_for_settlement.send_command(cmd) {
                        tracing::warn!(error = %e, "Failed to forward settlement command");
                    }
                }
            });

            // Run client service and node concurrently
            tokio::select! {
                _ = client_service.into_task() => {}
                _ = node.into_task() => {}
            }
        });

        info!("{} node built successfully", Client::NAME);
        Ok((task, providers))
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
    type Providers = crate::rpc::BootnodeRpcProviders;
    type Error = SwarmNodeError;

    async fn build(self, _ctx: &NodeContext) -> Result<(NodeTask, Self::Providers), Self::Error> {
        use tracing::info;
        use vertex_swarmspec::Loggable;

        info!("Building {} node...", Bootnode::NAME);
        self.spec.log();
        self.identity.log();
        info!("Peers database: {}", self.peers_path.display());

        // Build the BootNode (no client protocols)
        let node = vertex_swarm_node::BootNode::<crate::types::DefaultBootnodeTypes>::builder(
            self.identity.clone(),
        )
        .with_network_config(&self.network_config)
        .with_peer_store(self.peer_store)
        .build()
        .await
        .map_err(|e| SwarmNodeError::Build(e.to_string()))?;

        // Get topology for RPC
        let topology = node.kademlia_topology().clone();

        // Create RPC providers
        let providers = crate::rpc::BootnodeRpcProviders::new(topology);

        // Create the main event loop task
        let task: NodeTask = Box::pin(async move {
            node.into_task().await;
        });

        info!("{} node built successfully", Bootnode::NAME);
        Ok((task, providers))
    }
}
