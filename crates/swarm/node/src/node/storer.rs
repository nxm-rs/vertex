//! StorerNode - full Swarm node with storage and chunk synchronization.
//!
//! A [`StorerNode`] extends [`ClientNode`](super::ClientNode) with storage
//! capabilities: local chunk storage, chunk synchronization with neighbors,
//! and participation in the redistribution game.
//!
//! Use this for nodes that store and serve chunks in the Swarm network.

use std::sync::Arc;

use eyre::Result;
use libp2p::{Multiaddr, PeerId};
use nectar_primitives::SwarmAddress;
use tokio::sync::mpsc;
use tracing::info;
use vertex_swarm_api::SwarmNodeTypes;
use vertex_swarm_kademlia::{KademliaConfig, KademliaTopology};
use vertex_swarm_peermanager::{PeerManager, PeerStore};
use vertex_swarm_topology::TopologyCommand;
use vertex_tasks::SpawnableTask;

use super::builder::BuilderConfig;
use super::client::{ClientNode, ClientNodeBuilder};
use crate::protocol::{PseudosettleEvent, SwapEvent};
use crate::{ClientHandle, ClientService};

/// A full Swarm storer node with storage and chunk sync.
///
/// This extends [`ClientNode`](super::ClientNode) with:
/// - Local chunk storage
/// - Chunk synchronization with neighborhood peers
/// - Participation in the redistribution game
///
/// # Example
///
/// ```ignore
/// let (node, service, handle) = StorerNode::<MyTypes>::builder(identity)
///     .with_network_config(&config)
///     .build()
///     .await?;
///
/// // Spawn the service
/// executor.spawn(service.into_task());
///
/// // Run the node
/// node.into_task().await;
/// ```
pub struct StorerNode<N: SwarmNodeTypes> {
    /// The underlying client node.
    client: ClientNode<N>,
    // TODO: Add storage-specific components:
    // - local_store: Arc<dyn LocalStore>
    // - chunk_sync: ChunkSyncService
    // - redistribution: RedistributionService
}

impl<N: SwarmNodeTypes> StorerNode<N> {
    /// Create a builder for constructing a StorerNode.
    pub fn builder(identity: N::Identity) -> StorerNodeBuilder<N> {
        StorerNodeBuilder::new(identity)
    }

    /// Get the local peer ID.
    pub fn local_peer_id(&self) -> &PeerId {
        self.client.local_peer_id()
    }

    /// Get the overlay address.
    pub fn overlay_address(&self) -> SwarmAddress {
        self.client.overlay_address()
    }

    /// Get the swarm identity.
    pub fn identity(&self) -> &N::Identity {
        self.client.identity()
    }

    /// Get the peer manager.
    pub fn peer_manager(&self) -> &Arc<PeerManager> {
        self.client.peer_manager()
    }

    /// Get the Kademlia topology.
    pub fn kademlia_topology(&self) -> &Arc<KademliaTopology<N::Identity>> {
        self.client.kademlia_topology()
    }

    /// Send a topology command.
    pub fn topology_command(&mut self, command: TopologyCommand) {
        self.client.topology_command(command);
    }

    /// Dial peers from multiaddr strings.
    pub fn dial_addresses(&mut self, addrs: &[String]) -> usize {
        self.client.dial_addresses(addrs)
    }

    /// Start listening on configured addresses.
    pub fn start_listening(&mut self) -> Result<()> {
        self.client.start_listening()
    }

    /// Connect to bootnodes.
    pub async fn connect_bootnodes(&mut self) -> Result<usize> {
        self.client.connect_bootnodes().await
    }

    /// Run the network event loop.
    ///
    /// This runs the client node event loop and any storer-specific tasks.
    pub async fn run(self) -> Result<()> {
        info!("Starting storer node event loop");
        // TODO: spawn chunk sync and redistribution tasks
        self.client.run().await
    }

    /// Get the number of connected peers.
    pub fn connected_peers(&self) -> usize {
        self.client.connected_peers()
    }

    /// Check if we're connected to any peers.
    pub fn is_connected(&self) -> bool {
        self.client.is_connected()
    }
}

impl<N: SwarmNodeTypes> SpawnableTask for StorerNode<N> {
    async fn into_task(self) {
        if let Err(e) = self.run().await {
            tracing::error!(error = %e, "StorerNode error");
        }
    }
}

/// Builder for StorerNode.
pub struct StorerNodeBuilder<N: SwarmNodeTypes> {
    config: BuilderConfig<N>,
    pseudosettle_event_tx: Option<mpsc::UnboundedSender<PseudosettleEvent>>,
    swap_event_tx: Option<mpsc::UnboundedSender<SwapEvent>>,
    // TODO: Add storage-specific config:
    // - local_store_config: LocalStoreConfig
    // - chunk_sync_config: ChunkSyncConfig
    // - redistribution_config: RedistributionConfig
}

impl<N: SwarmNodeTypes> StorerNodeBuilder<N> {
    /// Create a new builder.
    pub fn new(identity: N::Identity) -> Self {
        Self {
            config: BuilderConfig::new(identity),
            pseudosettle_event_tx: None,
            swap_event_tx: None,
        }
    }

    /// Set network configuration.
    pub fn with_network_config(
        mut self,
        config: &impl vertex_swarm_api::SwarmNetworkConfig,
    ) -> Self {
        self.config.apply_network_config(config);
        self
    }

    /// Set the bootnodes.
    pub fn with_bootnodes(mut self, bootnodes: Vec<Multiaddr>) -> Self {
        self.config.bootnodes = bootnodes;
        self
    }

    /// Set the listen addresses.
    pub fn with_listen_addrs(mut self, addrs: Vec<Multiaddr>) -> Self {
        self.config.listen_addrs = addrs;
        self
    }

    /// Set the Kademlia configuration.
    pub fn with_kademlia_config(mut self, kademlia_config: KademliaConfig) -> Self {
        self.config.kademlia_config = kademlia_config;
        self
    }

    /// Set the peer store.
    pub fn with_peer_store(mut self, store: Arc<dyn PeerStore>) -> Self {
        self.config.peer_store = Some(store);
        self
    }

    /// Set the sender for routing pseudosettle events.
    pub fn with_pseudosettle_events(
        mut self,
        tx: mpsc::UnboundedSender<PseudosettleEvent>,
    ) -> Self {
        self.pseudosettle_event_tx = Some(tx);
        self
    }

    /// Set the sender for routing swap events.
    pub fn with_swap_events(mut self, tx: mpsc::UnboundedSender<SwapEvent>) -> Self {
        self.swap_event_tx = Some(tx);
        self
    }

    /// Build the StorerNode and ClientService.
    pub async fn build(self) -> Result<(StorerNode<N>, ClientService, ClientHandle)> {
        info!("Initializing storer P2P network...");

        // Build the underlying client node using the builder pattern
        let mut client_builder = ClientNodeBuilder::new(self.config.identity)
            .with_bootnodes(self.config.bootnodes)
            .with_listen_addrs(self.config.listen_addrs)
            .with_kademlia_config(self.config.kademlia_config);

        if let Some(store) = self.config.peer_store {
            client_builder = client_builder.with_peer_store(store);
        }

        if let Some(tx) = self.pseudosettle_event_tx {
            client_builder = client_builder.with_pseudosettle_events(tx);
        }
        if let Some(tx) = self.swap_event_tx {
            client_builder = client_builder.with_swap_events(tx);
        }

        let (client, service, handle) = client_builder.build().await?;

        // TODO: Initialize storage-specific components:
        // - local_store
        // - chunk_sync
        // - redistribution

        let node = StorerNode { client };

        Ok((node, service, handle))
    }
}
