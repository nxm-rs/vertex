//! Handle for querying and controlling topology state.

use std::sync::Arc;

use libp2p::Multiaddr;
use nectar_primitives::ChunkAddress;
use tokio::sync::{broadcast, mpsc};
use vertex_swarm_api::{SwarmIdentity, SwarmTopology};
use vertex_swarm_peermanager::PeerManager;
use vertex_swarm_primitives::OverlayAddress;

use crate::events::TopologyServiceEvent;
use crate::routing::{KademliaRouting, SwarmRouting};
use crate::{TopologyCommand, TopologyError};

/// Handle for querying topology state. Cheap to clone.
pub struct TopologyHandle<I: SwarmIdentity> {
    routing: Arc<KademliaRouting<I>>,
    peer_manager: Arc<PeerManager>,
    command_tx: mpsc::Sender<TopologyCommand>,
    event_tx: broadcast::Sender<TopologyServiceEvent>,
}

impl<I: SwarmIdentity> Clone for TopologyHandle<I> {
    fn clone(&self) -> Self {
        Self {
            routing: Arc::clone(&self.routing),
            peer_manager: Arc::clone(&self.peer_manager),
            command_tx: self.command_tx.clone(),
            event_tx: self.event_tx.clone(),
        }
    }
}

impl<I: SwarmIdentity> TopologyHandle<I> {
    pub(crate) fn new(
        routing: Arc<KademliaRouting<I>>,
        peer_manager: Arc<PeerManager>,
        command_tx: mpsc::Sender<TopologyCommand>,
        event_tx: broadcast::Sender<TopologyServiceEvent>,
    ) -> Self {
        Self {
            routing,
            peer_manager,
            command_tx,
            event_tx,
        }
    }

    /// Trigger connection to bootnodes and trusted peers.
    pub async fn connect_bootnodes(&self) -> Result<(), TopologyError> {
        self.command_tx
            .send(TopologyCommand::ConnectBootnodes)
            .await
            .map_err(|_| TopologyError::ServiceShutdown)
    }

    pub async fn dial(&self, addr: Multiaddr) -> Result<(), TopologyError> {
        self.command_tx
            .send(TopologyCommand::Dial { addr, for_gossip: false })
            .await
            .map_err(|_| TopologyError::ServiceShutdown)
    }

    pub async fn disconnect(&self, peer: OverlayAddress) -> Result<(), TopologyError> {
        self.command_tx
            .send(TopologyCommand::CloseConnection(peer))
            .await
            .map_err(|_| TopologyError::ServiceShutdown)
    }

    pub fn ban_peer(&self, peer: &OverlayAddress, reason: Option<String>) {
        self.peer_manager.ban(peer, reason);
        SwarmRouting::remove_peer(&*self.routing, peer);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<TopologyServiceEvent> {
        self.event_tx.subscribe()
    }

    pub fn peer_score(&self, peer: &OverlayAddress) -> f64 {
        self.peer_manager.peer_score(peer)
    }

    pub fn is_banned(&self, peer: &OverlayAddress) -> bool {
        self.peer_manager.is_banned(peer)
    }

    pub fn connected_peers(&self) -> Vec<OverlayAddress> {
        self.routing.connected_peers()
    }

    pub fn known_peers(&self) -> Vec<OverlayAddress> {
        self.routing.known_peers()
    }

    /// Access the underlying routing for SwarmRouting operations.
    pub fn routing(&self) -> &Arc<KademliaRouting<I>> {
        &self.routing
    }
}

impl<I: SwarmIdentity> std::fmt::Debug for TopologyHandle<I> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TopologyHandle")
            .field("depth", &SwarmTopology::depth(self))
            .field("connected_peers", &self.routing.connected_peers().len())
            .finish()
    }
}

impl<I: SwarmIdentity> SwarmTopology for TopologyHandle<I> {
    type Identity = I;

    fn identity(&self) -> &Self::Identity {
        SwarmTopology::identity(&*self.routing)
    }

    fn depth(&self) -> u8 {
        SwarmTopology::depth(&*self.routing)
    }

    fn neighbors(&self, depth: u8) -> Vec<OverlayAddress> {
        SwarmTopology::neighbors(&*self.routing, depth)
    }

    fn closest_to(&self, address: &ChunkAddress, count: usize) -> Vec<OverlayAddress> {
        SwarmTopology::closest_to(&*self.routing, address, count)
    }

    fn connected_peers_count(&self) -> usize {
        SwarmTopology::connected_peers_count(&*self.routing)
    }

    fn known_peers_count(&self) -> usize {
        SwarmTopology::known_peers_count(&*self.routing)
    }

    fn pending_connections_count(&self) -> usize {
        SwarmTopology::pending_connections_count(&*self.routing)
    }

    fn bin_sizes(&self) -> Vec<(usize, usize)> {
        SwarmTopology::bin_sizes(&*self.routing)
    }

    fn connected_peers_in_bin(&self, po: u8) -> Vec<String> {
        SwarmTopology::connected_peers_in_bin(&*self.routing, po)
    }
}
