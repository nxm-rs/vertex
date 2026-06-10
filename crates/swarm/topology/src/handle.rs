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

    /// Resolve once the node is connected to at least one storer, the
    /// deterministic point from which a chunk push or retrieval can route.
    ///
    /// Both [`SwarmChunkSender::send_chunk`](vertex_swarm_api::SwarmChunkSender)
    /// and [`SwarmChunkProvider::retrieve_chunk`](vertex_swarm_api::SwarmChunkProvider)
    /// pick the closest storers from the routing table; with none connected the
    /// push fails with `NoStorer` and the retrieval has nowhere to ask. This gate
    /// is state-driven, not timed: it returns as soon as a storer is present and
    /// otherwise waits for the [`TopologyEvent::PeerReady`] that brings one in.
    ///
    /// The subscription is taken before the initial state read so a storer that
    /// becomes ready between the two cannot be missed. Returns
    /// [`TopologyError::ServiceShutdown`] if the topology service stops before a
    /// storer connects.
    ///
    /// This is the minimal readiness condition for routing. A fuller surface
    /// (neighborhood saturation, target depth) is tracked for storer-side
    /// consumers; see the follow-up issue referenced from the chunk examples.
    pub async fn wait_until_routable(&self) -> Result<(), TopologyError> {
        let mut events = self.event_tx.subscribe();

        if self.connected_storer_count() > 0 {
            return Ok(());
        }

        loop {
            match events.recv().await {
                Ok(TopologyEvent::PeerReady { node_type, .. }) if node_type.requires_storage() => {
                    return Ok(());
                }
                Ok(_) => continue,
                // Lagged: events were dropped, so re-read state directly rather
                // than trust the stream. A storer may have connected in the gap.
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    if self.connected_storer_count() > 0 {
                        return Ok(());
                    }
                }
                Err(broadcast::error::RecvError::Closed) => {
                    return Err(TopologyError::ServiceShutdown);
                }
            }
        }
    }

    fn connected_storer_count(&self) -> u64 {
        self.metrics.connected_storers()
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
