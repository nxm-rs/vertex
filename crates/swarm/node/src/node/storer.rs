//! StorerNode - full Swarm node with storage and chunk synchronization.
//!
//! A [`StorerNode`] extends [`ClientNode`](super::ClientNode) with storage
//! capabilities: local chunk storage, chunk synchronization with neighbors,
//! and participation in the redistribution game.
//!
//! Use this for nodes that store and serve chunks in the Swarm network.

use eyre::Result;
use libp2p::PeerId;
use nectar_primitives::SwarmAddress;
use tokio::sync::mpsc;
use tracing::info;
use vertex_swarm_api::{SwarmIdentity, SwarmNetworkConfig, SwarmPeerConfig, SwarmRoutingConfig};
use vertex_swarm_topology::{KademliaConfig, TopologyCommand, TopologyHandle};
use vertex_tasks::GracefulShutdown;

use super::client::{ClientNode, ClientNodeBuilder};
use crate::protocol::PseudosettleEvent;
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
/// let (node, service, handle) = StorerNode::builder(identity)
///     .build(&config)
///     .await?;
///
/// // Spawn the service
/// executor.spawn(service.into_task());
///
/// // Run the node
/// node.into_task().await;
/// ```
pub struct StorerNode<I: SwarmIdentity + Clone> {
    /// The underlying client node.
    client: ClientNode<I>,
    // TODO: Add storage-specific components:
    // - local_store: Arc<dyn LocalStore>
    // - chunk_sync: ChunkSyncService
    // - redistribution: RedistributionService
}

impl<I: SwarmIdentity + Clone> StorerNode<I> {
    /// Create a builder for constructing a StorerNode.
    pub fn builder(identity: I) -> StorerNodeBuilder<I> {
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

    /// Get the topology handle for peer and routing queries.
    pub fn topology_handle(&self) -> &TopologyHandle<I> {
        self.client.topology_handle()
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

    /// Run the network event loop with graceful shutdown support.
    ///
    /// This runs the client node event loop and any storer-specific tasks.
    /// When the shutdown signal fires, the node will complete its current work
    /// and exit gracefully.
    pub async fn run(self, shutdown: GracefulShutdown) -> Result<()> {
        info!("Starting storer node event loop");
        // TODO: spawn chunk sync and redistribution tasks
        self.client.run(shutdown).await
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

/// Builder for StorerNode.
pub struct StorerNodeBuilder<I: SwarmIdentity + Clone> {
    identity: I,
    kademlia_config: Option<KademliaConfig>,
    pseudosettle_event_tx: Option<mpsc::UnboundedSender<PseudosettleEvent>>,
}

impl<I: SwarmIdentity + Clone> StorerNodeBuilder<I> {
    /// Create a new builder.
    pub fn new(identity: I) -> Self {
        Self {
            identity,
            kademlia_config: None,
            pseudosettle_event_tx: None,
        }
    }

    /// Set the Kademlia configuration.
    pub fn with_kademlia_config(mut self, kademlia_config: KademliaConfig) -> Self {
        self.kademlia_config = Some(kademlia_config);
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

}

impl<I: SwarmIdentity + Clone> StorerNodeBuilder<I> {
    /// Build the StorerNode and ClientService using the provided network configuration.
    pub async fn build<C>(
        self,
        network_config: &C,
        peer_store: Option<std::sync::Arc<dyn vertex_net_peer_store::NetPeerStore<vertex_swarm_peer_manager::StoredPeer>>>,
        score_store: Option<std::sync::Arc<dyn vertex_swarm_api::SwarmScoreStore<Score = vertex_swarm_peer_score::PeerScore, Error = vertex_net_peer_store::StoreError>>>,
    ) -> Result<(StorerNode<I>, ClientService, ClientHandle)>
    where
        I: vertex_swarm_spec::HasSpec,
        C: SwarmNetworkConfig + SwarmPeerConfig + SwarmRoutingConfig<Routing = KademliaConfig>,
    {
        info!("Initializing storer P2P network...");

        // Build the underlying client node using the builder pattern
        let mut client_builder = ClientNodeBuilder::new(self.identity);

        if let Some(kademlia) = self.kademlia_config {
            client_builder = client_builder.with_kademlia_config(kademlia);
        }
        if let Some(tx) = self.pseudosettle_event_tx {
            client_builder = client_builder.with_pseudosettle_events(tx);
        }

        let (client, service, handle) = client_builder.build(network_config, peer_store, score_store).await?;

        // TODO: Initialize storage-specific components:
        // - local_store
        // - chunk_sync
        // - redistribution

        let node = StorerNode { client };

        Ok((node, service, handle))
    }
}
