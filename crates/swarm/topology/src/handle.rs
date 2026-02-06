//! Read-only handle for querying topology state.
//!
//! [`TopologyHandle`] provides query methods for routing/peer state and commands
//! for dial/disconnect. It implements [`SwarmTopologyProvider`] for RPC integration.

use std::sync::Arc;

use libp2p::Multiaddr;
use nectar_primitives::ChunkAddress;
use tokio::sync::{broadcast, mpsc};
use vertex_swarm_api::{SwarmIdentity, SwarmTopology};
use vertex_swarm_peermanager::PeerManager;
use vertex_swarm_primitives::OverlayAddress;

use crate::dial_tracker::DialTracker;
use crate::events::TopologyServiceEvent;
use crate::routing::KademliaRouting;
use crate::{TopologyCommand, TopologyError};

/// Read-only handle for querying topology state. Cheap to clone.
///
/// Exposes Arc-wrapped components for direct access. For RPC integration,
/// this struct implements [`SwarmTopologyProvider`].
pub struct TopologyHandle<I: SwarmIdentity> {
    /// Kademlia routing table for peer discovery and routing.
    pub routing: Arc<KademliaRouting<I>>,
    /// Peer lifecycle management.
    pub peer_manager: Arc<PeerManager>,
    /// Dial attempt tracking (internal use only).
    dial_tracker: Arc<DialTracker>,
    command_tx: mpsc::Sender<TopologyCommand>,
    event_tx: broadcast::Sender<TopologyServiceEvent>,
}

// Manual Clone impl to avoid requiring I: Clone (all fields are Arc/Clone types)
impl<I: SwarmIdentity> Clone for TopologyHandle<I> {
    fn clone(&self) -> Self {
        Self {
            routing: Arc::clone(&self.routing),
            peer_manager: Arc::clone(&self.peer_manager),
            dial_tracker: Arc::clone(&self.dial_tracker),
            command_tx: self.command_tx.clone(),
            event_tx: self.event_tx.clone(),
        }
    }
}

impl<I: SwarmIdentity> TopologyHandle<I> {
    /// Create a new topology handle.
    pub fn new(
        routing: Arc<KademliaRouting<I>>,
        peer_manager: Arc<PeerManager>,
        dial_tracker: Arc<DialTracker>,
        command_tx: mpsc::Sender<TopologyCommand>,
        event_tx: broadcast::Sender<TopologyServiceEvent>,
    ) -> Self {
        Self {
            routing,
            peer_manager,
            dial_tracker,
            command_tx,
            event_tx,
        }
    }

    /// Get the node's identity.
    pub fn identity(&self) -> &I {
        self.routing.identity()
    }

    /// Get the current neighborhood depth.
    pub fn depth(&self) -> u8 {
        self.routing.depth()
    }

    /// Find peers closest to an address in overlay space.
    pub fn closest_to(&self, addr: &ChunkAddress, count: usize) -> Vec<OverlayAddress> {
        self.routing.closest_to(addr, count)
    }

    /// Request to dial a peer at the given address.
    pub async fn dial(&self, addr: Multiaddr) -> Result<(), TopologyError> {
        self.command_tx
            .send(TopologyCommand::Dial {
                addr,
                for_gossip: false,
            })
            .await
            .map_err(|_| TopologyError::ServiceShutdown)
    }

    /// Request to disconnect from a peer.
    pub async fn disconnect(&self, peer: OverlayAddress) -> Result<(), TopologyError> {
        self.command_tx
            .send(TopologyCommand::CloseConnection(peer))
            .await
            .map_err(|_| TopologyError::ServiceShutdown)
    }

    /// Ban a peer and remove from routing.
    pub fn ban_peer(&self, peer: &OverlayAddress, reason: Option<String>) {
        self.peer_manager.ban(peer, reason);
        self.routing.remove_peer(peer);
    }

    /// Subscribe to topology events.
    pub fn subscribe(&self) -> broadcast::Receiver<TopologyServiceEvent> {
        self.event_tx.subscribe()
    }
}

impl<I: SwarmIdentity> std::fmt::Debug for TopologyHandle<I> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TopologyHandle")
            .field("depth", &self.depth())
            .field("connected_peers", &self.routing.connected_peers().len())
            .finish()
    }
}

impl<I: SwarmIdentity> vertex_swarm_api::SwarmTopologyProvider for TopologyHandle<I> {
    fn overlay_address(&self) -> String {
        hex::encode(self.routing.identity().overlay_address().as_slice())
    }

    fn depth(&self) -> u8 {
        <KademliaRouting<I> as SwarmTopology>::depth(&self.routing)
    }

    fn connected_peers_count(&self) -> usize {
        self.routing.connected_peers().len()
    }

    fn known_peers_count(&self) -> usize {
        self.routing.known_peers().len()
    }

    fn pending_connections_count(&self) -> usize {
        self.dial_tracker.pending_count()
    }

    fn bin_sizes(&self) -> Vec<(usize, usize)> {
        self.routing.bin_sizes()
    }

    fn connected_peers_in_bin(&self, po: u8) -> Vec<String> {
        vertex_swarm_api::SwarmTopologyProvider::connected_peers_in_bin(&*self.routing, po)
    }
}
