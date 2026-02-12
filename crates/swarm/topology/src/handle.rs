//! Handle for querying and controlling topology state.

use std::sync::Arc;

use libp2p::Multiaddr;
use nectar_primitives::ChunkAddress;
use tokio::sync::{broadcast, mpsc};
use vertex_swarm_api::{SwarmIdentity, SwarmTopology, TopologyStats};
use vertex_swarm_peer_manager::PeerManager;
use vertex_swarm_primitives::OverlayAddress;

use vertex_swarm_peer_registry::SwarmPeerRegistry as ConnectionRegistry;
use crate::events::TopologyEvent;
use crate::routing::KademliaRouting;
use crate::{TopologyCommand, TopologyError};

/// Handle for querying topology state. Cheap to clone.
pub struct TopologyHandle<I: SwarmIdentity> {
    identity: Arc<I>,
    routing: Arc<KademliaRouting<I>>,
    connection_registry: Arc<ConnectionRegistry>,
    peer_manager: Arc<PeerManager>,
    command_tx: mpsc::Sender<TopologyCommand>,
    event_tx: broadcast::Sender<TopologyEvent>,
}

impl<I: SwarmIdentity> Clone for TopologyHandle<I> {
    fn clone(&self) -> Self {
        Self {
            identity: Arc::clone(&self.identity),
            routing: Arc::clone(&self.routing),
            connection_registry: Arc::clone(&self.connection_registry),
            peer_manager: Arc::clone(&self.peer_manager),
            command_tx: self.command_tx.clone(),
            event_tx: self.event_tx.clone(),
        }
    }
}

impl<I: SwarmIdentity> TopologyHandle<I> {
    pub(crate) fn new(
        identity: Arc<I>,
        routing: Arc<KademliaRouting<I>>,
        connection_registry: Arc<ConnectionRegistry>,
        peer_manager: Arc<PeerManager>,
        command_tx: mpsc::Sender<TopologyCommand>,
        event_tx: broadcast::Sender<TopologyEvent>,
    ) -> Self {
        Self {
            identity,
            routing,
            connection_registry,
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
            .send(TopologyCommand::Dial(addr))
            .await
            .map_err(|_| TopologyError::ServiceShutdown)
    }

    pub async fn disconnect(&self, peer: OverlayAddress) -> Result<(), TopologyError> {
        self.command_tx
            .send(TopologyCommand::CloseConnection(peer))
            .await
            .map_err(|_| TopologyError::ServiceShutdown)
    }

    /// Ban a peer and remove from routing.
    pub async fn ban_peer(
        &self,
        peer: OverlayAddress,
        reason: Option<String>,
    ) -> Result<(), TopologyError> {
        self.command_tx
            .send(TopologyCommand::BanPeer {
                overlay: peer,
                reason,
            })
            .await
            .map_err(|_| TopologyError::ServiceShutdown)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<TopologyEvent> {
        self.event_tx.subscribe()
    }

    /// Get direct access to the peer manager for scoring/banning queries.
    pub fn peer_manager(&self) -> &Arc<PeerManager> {
        &self.peer_manager
    }

    /// Store agent version from libp2p identify protocol.
    pub fn set_agent_version(&self, peer_id: &libp2p::PeerId, agent_version: String) {
        self.connection_registry.set_agent_version(peer_id, agent_version);
    }

    /// Get agent version for a peer by PeerId.
    pub fn agent_version(&self, peer_id: &libp2p::PeerId) -> Option<String> {
        self.connection_registry.agent_version(peer_id)
    }

    /// Get agent version for a peer by overlay address.
    pub fn agent_version_by_overlay(&self, overlay: &OverlayAddress) -> Option<String> {
        self.connection_registry.agent_version_by_overlay(overlay)
    }
}

impl<I: SwarmIdentity> SwarmTopology for TopologyHandle<I> {
    type Identity = I;

    fn identity(&self) -> &Self::Identity {
        &self.identity
    }

    fn depth(&self) -> u8 {
        self.routing.depth()
    }

    fn neighbors(&self, depth: u8) -> Vec<OverlayAddress> {
        self.routing.neighbors(depth)
    }

    fn closest_to(&self, address: &ChunkAddress, count: usize) -> Vec<OverlayAddress> {
        self.routing.closest_to(address, count)
    }

    fn bin_sizes(&self) -> Vec<(usize, usize)> {
        self.routing.bin_sizes()
    }

    fn connected_peers_in_bin(&self, po: u8) -> Vec<String> {
        self.routing.connected_peers_in_bin(po)
    }
}

impl<I: SwarmIdentity> TopologyStats for TopologyHandle<I> {
    fn connected_peers_count(&self) -> usize {
        self.connection_registry.active_count()
    }

    fn known_peers_count(&self) -> usize {
        self.peer_manager.known_peers_count()
    }

    fn pending_connections_count(&self) -> usize {
        self.connection_registry.pending_count()
    }
}
