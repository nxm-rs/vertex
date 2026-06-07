//! Handle for querying and controlling topology state.

use std::sync::Arc;

use libp2p::{Multiaddr, PeerId};
use nectar_primitives::ChunkAddress;
use tokio::sync::{broadcast, mpsc};
use vertex_swarm_api::{
    SwarmIdentity, SwarmTopologyBins, SwarmTopologyCommands, SwarmTopologyPeers,
    SwarmTopologyRouting, SwarmTopologyState, SwarmTopologyStats,
};
use vertex_swarm_net_identify as identify;
use vertex_swarm_peer_manager::PeerManager;
use vertex_swarm_primitives::{Bin, NeighborhoodDepth, OverlayAddress};

use crate::behaviour::ConnectionRegistry;
use crate::events::TopologyEvent;
use crate::kademlia::KademliaRouting;
use crate::metrics::TopologyMetrics;
use crate::{TopologyCommand, TopologyError};

/// Handle for querying topology state. Cheap to clone.
pub struct TopologyHandle<I: SwarmIdentity> {
    identity: Arc<I>,
    routing: Arc<KademliaRouting<I>>,
    connection_registry: Arc<ConnectionRegistry>,
    peer_manager: Arc<PeerManager<I>>,
    command_tx: mpsc::Sender<TopologyCommand>,
    event_tx: broadcast::Sender<TopologyEvent>,
    agent_versions: identify::AgentVersions,
    metrics: Arc<TopologyMetrics>,
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
            agent_versions: Arc::clone(&self.agent_versions),
            metrics: Arc::clone(&self.metrics),
        }
    }
}

impl<I: SwarmIdentity> TopologyHandle<I> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        identity: Arc<I>,
        routing: Arc<KademliaRouting<I>>,
        connection_registry: Arc<ConnectionRegistry>,
        peer_manager: Arc<PeerManager<I>>,
        command_tx: mpsc::Sender<TopologyCommand>,
        event_tx: broadcast::Sender<TopologyEvent>,
        agent_versions: identify::AgentVersions,
        metrics: Arc<TopologyMetrics>,
    ) -> Self {
        Self {
            identity,
            routing,
            connection_registry,
            peer_manager,
            command_tx,
            event_tx,
            agent_versions,
            metrics,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<TopologyEvent> {
        self.event_tx.subscribe()
    }

    /// Get direct access to the peer manager for scoring/banning queries.
    pub fn peer_manager(&self) -> &Arc<PeerManager<I>> {
        &self.peer_manager
    }

    /// Get agent version for a peer by PeerId.
    pub fn agent_version(&self, peer_id: &PeerId) -> Option<String> {
        self.agent_versions.read().peek(peer_id).cloned()
    }

    /// Get agent version for a peer by overlay address.
    pub fn agent_version_by_overlay(&self, overlay: &OverlayAddress) -> Option<String> {
        let peer_id = self.connection_registry.resolve_peer_id(overlay)?;
        self.agent_versions.read().peek(&peer_id).cloned()
    }
}

impl<I: SwarmIdentity> SwarmTopologyBins for TopologyHandle<I> {
    fn bin_sizes(&self) -> Vec<(usize, usize)> {
        self.routing.bin_sizes()
    }
}

impl<I: SwarmIdentity> SwarmTopologyState for TopologyHandle<I> {
    type Identity = I;

    fn identity(&self) -> &Self::Identity {
        &self.identity
    }

    fn depth(&self) -> NeighborhoodDepth {
        self.routing.depth()
    }
}

impl<I: SwarmIdentity> SwarmTopologyRouting for TopologyHandle<I> {
    fn closest_to(&self, address: &ChunkAddress, count: usize) -> Vec<OverlayAddress> {
        self.routing.closest_to(address, count)
    }

    fn neighbors(&self, depth: NeighborhoodDepth) -> Vec<OverlayAddress> {
        self.routing.neighbors(depth)
    }
}

impl<I: SwarmIdentity> SwarmTopologyPeers for TopologyHandle<I> {
    fn connected_peers_in_bin(&self, bin: Bin) -> Vec<OverlayAddress> {
        self.routing.connected_overlays_in_bin(bin)
    }

    fn connected_peer_details_in_bin(
        &self,
        bin: Bin,
    ) -> Vec<(OverlayAddress, Vec<libp2p::Multiaddr>)> {
        self.routing
            .connected_overlays_in_bin(bin)
            .into_iter()
            .map(|overlay| {
                let multiaddrs = self
                    .peer_manager
                    .get_swarm_peer(&overlay)
                    .map(|p| p.multiaddrs().to_vec())
                    .unwrap_or_default();
                (overlay, multiaddrs)
            })
            .collect()
    }
}

impl<I: SwarmIdentity> SwarmTopologyStats for TopologyHandle<I> {
    fn connected_peers_count(&self) -> usize {
        (self.metrics.connected_storers() + self.metrics.connected_clients()) as usize
    }

    fn routing_peers_count(&self) -> usize {
        self.peer_manager.index().len()
    }

    fn pending_connections_count(&self) -> usize {
        self.connection_registry.pending_count()
    }

    fn stored_peers_count(&self) -> usize {
        self.peer_manager.stored_count()
    }
}

#[async_trait::async_trait]
impl<I: SwarmIdentity> SwarmTopologyCommands for TopologyHandle<I> {
    type Error = TopologyError;

    async fn connect_bootnodes(&self) -> Result<(), TopologyError> {
        self.command_tx
            .send(TopologyCommand::ConnectBootnodes)
            .await
            .map_err(|_| TopologyError::ServiceShutdown)
    }

    async fn dial(&self, addr: Multiaddr) -> Result<(), TopologyError> {
        self.command_tx
            .send(TopologyCommand::Dial(addr))
            .await
            .map_err(|_| TopologyError::ServiceShutdown)
    }

    async fn disconnect(&self, peer: OverlayAddress) -> Result<(), TopologyError> {
        self.command_tx
            .send(TopologyCommand::CloseConnection(peer))
            .await
            .map_err(|_| TopologyError::ServiceShutdown)
    }

    async fn ban_peer(
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

    async fn save_peers(&self) -> Result<(), TopologyError> {
        self.command_tx
            .send(TopologyCommand::SavePeers)
            .await
            .map_err(|_| TopologyError::ServiceShutdown)
    }
}

/// Detailed routing statistics.
#[derive(Debug, Clone)]
pub struct RoutingStats {
    pub bins: Vec<BinStats>,
    pub depth: u8,
    pub known_peers_total: usize,
    pub connected_peers_total: usize,
}

#[derive(Debug, Clone)]
pub struct BinStats {
    pub bin: u8,
    pub connected: usize,
    pub known: usize,
    pub dialing: usize,
    pub handshaking: usize,
    pub active: usize,
    /// Target allocation from linear taper formula. `usize::MAX` for neighborhood bins.
    pub target: usize,
    /// Target + inbound headroom (max before rejecting inbound). `usize::MAX` for neighborhood bins.
    pub ceiling: usize,
    pub nominal: usize,
}

impl<I: SwarmIdentity> TopologyHandle<I> {
    /// Get detailed routing statistics for metrics.
    pub fn routing_stats(&self) -> RoutingStats {
        let bin_sizes = self.routing.bin_sizes();
        let bin_phases = self.routing.all_bin_phases();
        let limits = self.routing.limits();
        let depth = self.routing.depth();

        let bins: Vec<BinStats> = bin_sizes
            .iter()
            .enumerate()
            .map(|(idx, (connected, known))| {
                let bin = Bin::new(idx as u8).unwrap_or(Bin::MAX);
                let (dialing, handshaking, active) = bin_phases
                    .get(idx)
                    .map(|(_, d, h, a)| (*d, *h, *a))
                    .unwrap_or((0, 0, 0));
                let target = limits.target(bin, depth);
                let ceiling = limits.ceiling(bin, depth);
                BinStats {
                    bin: idx as u8,
                    connected: *connected,
                    known: *known,
                    dialing,
                    handshaking,
                    active,
                    target,
                    ceiling,
                    nominal: limits.nominal(),
                }
            })
            .collect();

        RoutingStats {
            bins,
            depth: depth.get(),
            known_peers_total: self.routing.known_peers_total(),
            connected_peers_total: self.routing.connected_peers_total(),
        }
    }
}
