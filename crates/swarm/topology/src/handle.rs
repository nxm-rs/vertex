//! Handle for querying and controlling topology state.

use std::sync::Arc;

use libp2p::{Multiaddr, PeerId};
use nectar_primitives::ChunkAddress;
use tokio::sync::{broadcast, mpsc};
use vertex_swarm_api::{
    PeerReporter, SwarmIdentity, SwarmSpec, SwarmTopologyBins, SwarmTopologyCommands,
    SwarmTopologyPeers, SwarmTopologyReporting, SwarmTopologyRouting, SwarmTopologyState,
    SwarmTopologyStats,
};
use vertex_swarm_net_identify as identify;
use vertex_swarm_peer_manager::PeerManager;
use vertex_swarm_primitives::{Bin, NeighborhoodDepth, OverlayAddress, all_bins};

use crate::behaviour::ConnectionRegistry;
use crate::events::TopologyEvent;
use crate::kademlia::KademliaRouting;
use crate::readiness::{BinReadiness, ReadinessSnapshot};
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
        }
    }
}

impl<I: SwarmIdentity> TopologyHandle<I> {
    pub(crate) fn new(
        identity: Arc<I>,
        routing: Arc<KademliaRouting<I>>,
        connection_registry: Arc<ConnectionRegistry>,
        peer_manager: Arc<PeerManager<I>>,
        command_tx: mpsc::Sender<TopologyCommand>,
        event_tx: broadcast::Sender<TopologyEvent>,
        agent_versions: identify::AgentVersions,
    ) -> Self {
        Self {
            identity,
            routing,
            connection_registry,
            peer_manager,
            command_tx,
            event_tx,
            agent_versions,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<TopologyEvent> {
        self.event_tx.subscribe()
    }

    /// Take a readiness snapshot from authoritative topology state.
    ///
    /// Counts come from the routing table's connected-peer index and the
    /// peer manager's handshake-confirmed node types; per-bin targets come
    /// from the depth-aware limits at the current depth. See
    /// [`ReadinessSnapshot`] for field semantics and consistency caveats.
    pub fn readiness(&self) -> ReadinessSnapshot {
        let depth = self.routing.depth();
        let limits = self.routing.limits();
        let max_bin = self.routing.max_bin();
        let saturation_threshold = self.identity.spec().saturation_peers() as usize;

        let mut connected_peers = 0;
        let mut neighborhood_connected = 0;
        let mut bins_at_target = 0;
        let bins: Vec<BinReadiness> = all_bins(max_bin)
            .map(|bin| {
                let (connected, _known) = self.routing.bin_peer_counts(bin);
                connected_peers += connected;
                if depth.contains(bin) {
                    neighborhood_connected += connected;
                }
                let raw_target = limits.target(bin, depth);
                let (target, deficit) = if raw_target == usize::MAX {
                    // Neighborhood bins connect to every available peer.
                    (None, 0)
                } else {
                    if connected >= raw_target {
                        bins_at_target += 1;
                    }
                    (Some(raw_target), raw_target.saturating_sub(connected))
                };
                BinReadiness {
                    bin,
                    connected,
                    target,
                    deficit,
                }
            })
            .collect();

        let (phase, time_in_phase) = self.routing.phase_status();

        ReadinessSnapshot {
            local_node_type: self.identity.node_type(),
            connected_peers,
            connected_storers: self.routing.connected_storer_total(),
            depth,
            neighborhood_connected,
            saturation_threshold,
            bins,
            bins_at_target,
            neighborhood_stable_for: self.routing.neighborhood_stable_for(),
            neighborhood_stability_window: self.routing.neighborhood_stability_window(),
            phase,
            time_in_phase,
        }
    }

    /// Resolve once `predicate` holds for a fresh [`ReadinessSnapshot`].
    ///
    /// State-driven, not timed: the predicate is evaluated immediately and
    /// then re-evaluated on every [`TopologyEvent::PeerReady`],
    /// [`TopologyEvent::PeerDisconnected`], [`TopologyEvent::DepthChanged`],
    /// and [`TopologyEvent::PhaseChanged`], the events that change snapshot
    /// state. The subscription is taken before the initial evaluation so a
    /// state change between the two cannot be missed.
    ///
    /// If the event subscription lags (events dropped under burst), the
    /// predicate is re-evaluated against current state unconditionally
    /// rather than trusting the stream, so a missed event cannot strand the
    /// waiter. Cancel-safe: dropping the returned future drops the
    /// subscription and leaks nothing. Returns
    /// [`TopologyError::ServiceShutdown`] if the topology event channel
    /// closes before the predicate holds.
    pub async fn wait_until(
        &self,
        mut predicate: impl FnMut(&ReadinessSnapshot) -> bool,
    ) -> Result<(), TopologyError> {
        let mut events = self.event_tx.subscribe();

        if predicate(&self.readiness()) {
            return Ok(());
        }

        loop {
            match events.recv().await {
                Ok(
                    TopologyEvent::PeerReady { .. }
                    | TopologyEvent::PeerDisconnected { .. }
                    | TopologyEvent::DepthChanged { .. }
                    | TopologyEvent::PhaseChanged { .. },
                ) => {
                    if predicate(&self.readiness()) {
                        return Ok(());
                    }
                }
                Ok(_) => continue,
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    if predicate(&self.readiness()) {
                        return Ok(());
                    }
                }
                Err(broadcast::error::RecvError::Closed) => {
                    return Err(TopologyError::ServiceShutdown);
                }
            }
        }
    }

    /// Resolve once the node is connected to at least one storer, the
    /// deterministic point from which a chunk push or retrieval can route.
    ///
    /// Both [`SwarmChunkSender::send_chunk`](vertex_swarm_api::SwarmChunkSender)
    /// and [`SwarmChunkProvider::retrieve_chunk`](vertex_swarm_api::SwarmChunkProvider)
    /// pick the closest storers from the routing table; with none connected the
    /// push fails with `NoStorer` and the retrieval has nowhere to ask. This is
    /// the minimal readiness condition; see [`Self::wait_until_ready`] for the
    /// composite warm gate and [`Self::wait_until`] for the event semantics.
    pub async fn wait_until_routable(&self) -> Result<(), TopologyError> {
        self.wait_until(ReadinessSnapshot::is_routable).await
    }

    /// Resolve once the neighborhood depth reaches `min_depth`.
    ///
    /// Driven by [`TopologyEvent::DepthChanged`]; see [`Self::wait_until`]
    /// for the event semantics.
    pub async fn wait_until_depth(
        &self,
        min_depth: NeighborhoodDepth,
    ) -> Result<(), TopologyError> {
        self.wait_until(move |s| s.depth_reached(min_depth)).await
    }

    /// Resolve once the neighborhood is saturated: a depth boundary is
    /// established and the bins at or beyond it hold at least the spec's
    /// per-bin saturation target in connected peers.
    ///
    /// See [`ReadinessSnapshot::is_saturated`] for the condition and
    /// [`Self::wait_until`] for the event semantics.
    pub async fn wait_until_saturated(&self) -> Result<(), TopologyError> {
        self.wait_until(ReadinessSnapshot::is_saturated).await
    }

    /// Resolve once the node is warm for its node type: routable for
    /// clients and bootnodes, routable plus neighborhood-saturated for
    /// storers.
    ///
    /// See [`ReadinessSnapshot::is_warm`] for the condition and
    /// [`Self::wait_until`] for the event semantics.
    pub async fn wait_until_ready(&self) -> Result<(), TopologyError> {
        self.wait_until(ReadinessSnapshot::is_warm).await
    }

    /// Resolve once the neighborhood is ready for pull-syncing:
    /// continuously saturated at an unchanged depth for the configured
    /// stability window (`KademliaConfig::with_neighborhood_stability_window`).
    ///
    /// See [`ReadinessSnapshot::is_neighborhood_ready`] for the condition
    /// and why chunk synchronization gates on it. Unlike the purely
    /// event-driven waits, this condition can also become true by time
    /// alone, so while the neighborhood is saturated a timer is armed for
    /// the remainder of the window alongside the event subscription; every
    /// wake (event, lag, or timer) re-evaluates against fresh state, so a
    /// missed event cannot strand the waiter. Cancel-safe: dropping the
    /// returned future drops the subscription and the timer. Returns
    /// [`TopologyError::ServiceShutdown`] if the topology event channel
    /// closes before the condition holds.
    pub async fn wait_until_neighborhood_ready(&self) -> Result<(), TopologyError> {
        let mut events = self.event_tx.subscribe();

        loop {
            let snapshot = self.readiness();
            if snapshot.is_neighborhood_ready() {
                return Ok(());
            }

            // Saturated but the window has not been served yet: arm a timer
            // for the remainder. Below saturation only a state change (an
            // event) can make progress, so park on the subscription alone.
            let remaining = snapshot.neighborhood_stable_for.map(|stable| {
                snapshot
                    .neighborhood_stability_window
                    .saturating_sub(stable)
            });
            let window_elapsed = async {
                match remaining {
                    Some(remaining) => vertex_tasks::time::sleep(remaining).await,
                    None => std::future::pending().await,
                }
            };
            tokio::pin!(window_elapsed);

            // Park until a wake that can change the verdict: the armed
            // timer, an event that changes snapshot state, or a lagged
            // stream (which may have dropped one). Other events park again
            // without rebuilding the snapshot or re-arming the timer.
            loop {
                tokio::select! {
                    () = &mut window_elapsed => break,
                    event = events.recv() => match event {
                        Ok(
                            TopologyEvent::PeerReady { .. }
                            | TopologyEvent::PeerDisconnected { .. }
                            | TopologyEvent::DepthChanged { .. },
                        )
                        | Err(broadcast::error::RecvError::Lagged(_)) => break,
                        Ok(_) => {}
                        Err(broadcast::error::RecvError::Closed) => {
                            return Err(TopologyError::ServiceShutdown);
                        }
                    },
                }
            }
        }
    }

    /// Get direct access to the peer manager for scoring/banning queries.
    pub fn peer_manager(&self) -> &Arc<PeerManager<I>> {
        &self.peer_manager
    }

    /// Resolve a connected peer's libp2p [`PeerId`] from its overlay, or `None`
    /// if not currently connected. Used by the pullsync puller to address its
    /// outbound substreams.
    pub fn resolve_peer_id(&self, overlay: &OverlayAddress) -> Option<PeerId> {
        self.connection_registry.resolve_peer_id(overlay)
    }

    /// The deepest bin the routing table tracks. The pullsync puller scopes its
    /// neighbourhood bins to this so it never drives ranges for bins the table
    /// cannot hold.
    pub fn max_bin(&self) -> Bin {
        self.routing.max_bin()
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

    fn neighbourhood_credible(&self) -> bool {
        self.readiness().is_saturated()
    }
}

impl<I: SwarmIdentity> SwarmTopologyReporting for TopologyHandle<I> {
    fn reporter(&self) -> Arc<dyn PeerReporter> {
        Arc::clone(self.peer_manager()) as Arc<dyn PeerReporter>
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
        self.routing.connected_peers_total()
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

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing)]
    use std::time::Duration;

    use super::*;
    use crate::behaviour::ConnectionRegistry;
    use crate::kademlia::{KademliaConfig, SwarmRouting};
    use vertex_net_peer_registry::ConnectionDirection;
    use vertex_swarm_peer_manager::{PeerManagerConfig, TrustLevel};
    use vertex_swarm_primitives::SwarmNodeType;
    use vertex_swarm_test_utils::{MockIdentity, test_overlay, test_peer_id, test_swarm_peer};

    struct ReadinessHarness {
        handle: TopologyHandle<MockIdentity>,
        routing: Arc<KademliaRouting<MockIdentity>>,
        peer_manager: Arc<PeerManager<MockIdentity>>,
        event_tx: broadcast::Sender<TopologyEvent>,
        // Held so handle commands stay sendable; unused by these tests.
        _command_rx: mpsc::Receiver<TopologyCommand>,
    }

    fn harness(node_type: SwarmNodeType, event_capacity: usize) -> ReadinessHarness {
        let identity = MockIdentity::with_overlay(test_overlay(0)).with_node_type(node_type);
        let peer_manager = PeerManager::new(&identity, PeerManagerConfig::default());
        let routing = KademliaRouting::new(
            identity.clone(),
            KademliaConfig::default(),
            peer_manager.clone(),
        );
        let (event_tx, _) = broadcast::channel(event_capacity);
        let (command_tx, command_rx) = mpsc::channel(8);
        let handle = TopologyHandle::new(
            Arc::new(identity),
            routing.clone(),
            Arc::new(ConnectionRegistry::new()),
            peer_manager.clone(),
            command_tx,
            event_tx.clone(),
            identify::new_agent_versions(),
        );
        ReadinessHarness {
            handle,
            routing,
            peer_manager,
            event_tx,
            _command_rx: command_rx,
        }
    }

    impl ReadinessHarness {
        /// Connect a peer through the same path the behaviour uses: record
        /// it in the peer manager, then in the routing table.
        fn connect(&self, n: u8, node_type: SwarmNodeType) -> OverlayAddress {
            self.peer_manager.on_peer_connected(
                test_swarm_peer(n),
                node_type,
                ConnectionDirection::Outbound,
                TrustLevel::Normal,
                None,
            );
            let overlay = test_overlay(n);
            SwarmRouting::connected(&*self.routing, overlay);
            overlay
        }

        fn emit_peer_ready(&self, n: u8, node_type: SwarmNodeType) {
            let _ = self.event_tx.send(TopologyEvent::PeerReady {
                overlay: test_overlay(n),
                peer_id: test_peer_id(n),
                node_type,
                direction: ConnectionDirection::Outbound,
            });
        }

        fn emit_ping(&self, n: u8) {
            let _ = self.event_tx.send(TopologyEvent::PingCompleted {
                overlay: test_overlay(n),
                rtt: Duration::from_millis(1),
            });
        }
    }

    #[test]
    fn empty_snapshot_is_cold_and_complete() {
        let h = harness(SwarmNodeType::Client, 16);
        let s = h.handle.readiness();

        assert_eq!(s.local_node_type, SwarmNodeType::Client);
        assert_eq!(s.connected_peers, 0);
        assert_eq!(s.connected_storers, 0);
        assert_eq!(s.depth, NeighborhoodDepth::ZERO);
        assert_eq!(s.neighborhood_connected, 0);
        assert!(!s.is_routable());
        assert!(!s.is_saturated());
        assert!(!s.is_warm());
        assert_eq!(s.bins_at_target, 0);
        assert_eq!(s.phase, crate::TopologyPhase::Bootstrap);

        // Every bin reports the bootstrap-phase target while depth is 0.
        assert!(!s.bins.is_empty());
        for bin in &s.bins {
            assert_eq!(bin.connected, 0);
            let target = bin.target.expect("finite target at depth 0");
            assert!(target > 0);
            assert_eq!(bin.deficit, target);
        }
    }

    #[test]
    fn snapshot_counts_storers_exactly() {
        let h = harness(SwarmNodeType::Client, 16);
        h.connect(1, SwarmNodeType::Storer);
        h.connect(2, SwarmNodeType::Storer);
        h.connect(3, SwarmNodeType::Client);

        let s = h.handle.readiness();
        assert_eq!(s.connected_peers, 3);
        assert_eq!(s.connected_storers, 2);
        assert!(s.is_routable());
        assert!(s.is_warm(), "client is warm once routable");

        let per_bin_total: usize = s.bins.iter().map(|b| b.connected).sum();
        assert_eq!(per_bin_total, s.connected_peers);
        // Depth 0: the whole table is the neighborhood.
        assert_eq!(s.neighborhood_connected, s.connected_peers);
    }

    /// Saturate bin 0 (8 peers at proximity order 0) while keeping bin 1
    /// unsaturated, anchoring depth at 1, and connect enough peers at or
    /// beyond the boundary to cross the neighborhood saturation threshold.
    fn saturate_to_depth_one(h: &ReadinessHarness) {
        // First byte 0x80..=0x87: proximity order 0 to the local 0x00 overlay.
        for n in 0x80..0x88 {
            h.connect(n, SwarmNodeType::Storer);
        }
        // Bin 1 (0x40..): 5 peers, below the per-bin saturation of 8.
        for n in 0x40..0x45 {
            h.connect(n, SwarmNodeType::Storer);
        }
        // Bin 2 (0x20..): 3 peers. Bin 3 (0x10): 1 peer.
        for n in 0x20..0x23 {
            h.connect(n, SwarmNodeType::Storer);
        }
        h.connect(0x10, SwarmNodeType::Storer);
    }

    #[test]
    fn snapshot_reports_depth_and_saturation() {
        let h = harness(SwarmNodeType::Storer, 16);
        saturate_to_depth_one(&h);

        let s = h.handle.readiness();
        assert_eq!(s.depth.get(), 1);
        // Bins at or beyond depth 1: 5 + 3 + 1 = 9 connected.
        assert_eq!(s.neighborhood_connected, 9);
        assert!(s.is_saturated());
        assert!(s.is_warm(), "storer is warm once saturated and routable");
        // Bin 0 is balanced (below depth) and holds 8 of its target.
        let bin0 = &s.bins[0];
        assert_eq!(bin0.connected, 8);
        assert!(bin0.target.is_some());
        // Neighborhood bins report no finite target.
        assert!(s.bins[1].target.is_none());

        // The phase machine sees the depth climb on its next evaluation
        // (depth moved within the stability window: Converging) and the
        // snapshot exposes the committed phase.
        h.routing
            .evaluate_phase()
            .expect("depth climb commits a phase transition");
        let s = h.handle.readiness();
        assert_eq!(s.phase, crate::TopologyPhase::Converging);
    }

    #[tokio::test]
    async fn wait_until_routable_resolves_on_peer_ready_event() {
        let h = harness(SwarmNodeType::Client, 16);

        let waiter = {
            let handle = h.handle.clone();
            tokio::spawn(async move { handle.wait_until_routable().await })
        };
        // Let the waiter subscribe and park on the event stream.
        tokio::task::yield_now().await;

        h.connect(1, SwarmNodeType::Storer);
        h.emit_peer_ready(1, SwarmNodeType::Storer);

        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("waiter must resolve on the triggering event")
            .expect("waiter task must not panic")
            .expect("wait_until_routable must succeed");
    }

    #[tokio::test]
    async fn wait_until_ready_for_storer_requires_saturation() {
        let h = harness(SwarmNodeType::Storer, 64);

        let mut waiter = Box::pin(h.handle.wait_until_ready());
        assert!(futures::poll!(waiter.as_mut()).is_pending());

        // One storer makes the node routable but not saturated: the storer
        // warm gate must hold out.
        h.connect(1, SwarmNodeType::Storer);
        h.emit_peer_ready(1, SwarmNodeType::Storer);
        assert!(futures::poll!(waiter.as_mut()).is_pending());

        saturate_to_depth_one(&h);
        h.emit_peer_ready(0x80, SwarmNodeType::Storer);

        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("storer warm gate must resolve once saturated")
            .expect("wait_until_ready must succeed");
    }

    #[tokio::test]
    async fn wait_until_depth_resolves_on_depth_change() {
        let h = harness(SwarmNodeType::Storer, 64);
        let min_depth = NeighborhoodDepth::new(Bin::new(1).expect("valid bin"));

        let mut waiter = Box::pin(h.handle.wait_until_depth(min_depth));
        assert!(futures::poll!(waiter.as_mut()).is_pending());

        saturate_to_depth_one(&h);
        let _ = h.event_tx.send(TopologyEvent::DepthChanged {
            old_depth: 0,
            new_depth: 1,
        });

        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("depth gate must resolve on DepthChanged")
            .expect("wait_until_depth must succeed");
    }

    #[tokio::test(start_paused = true)]
    async fn neighborhood_ready_requires_stability_window() {
        let h = harness(SwarmNodeType::Storer, 64);
        assert!(h.handle.readiness().neighborhood_stable_for.is_none());

        saturate_to_depth_one(&h);
        let s = h.handle.readiness();
        assert!(s.is_saturated());
        let stable = s
            .neighborhood_stable_for
            .expect("saturated neighborhood must track stability");
        assert!(stable < s.neighborhood_stability_window);
        assert!(!s.is_neighborhood_ready(), "window not served yet");

        tokio::time::advance(s.neighborhood_stability_window).await;
        assert!(h.handle.readiness().is_neighborhood_ready());
    }

    #[tokio::test(start_paused = true)]
    async fn neighborhood_stability_resets_on_saturation_dip() {
        let h = harness(SwarmNodeType::Storer, 64);
        saturate_to_depth_one(&h);
        tokio::time::advance(Duration::from_secs(20)).await;

        // Drop the neighborhood (9 connected at depth 1) below the
        // threshold (8): the clock must clear, not pause.
        for n in [0x40, 0x41] {
            let overlay = test_overlay(n);
            SwarmRouting::on_peer_disconnected(&*h.routing, &overlay);
            h.peer_manager.on_peer_disconnected(&overlay, "test");
        }
        assert!(
            h.handle.readiness().neighborhood_stable_for.is_none(),
            "saturation dip must clear the stability clock"
        );

        // Recovering restarts the clock from zero, not the pre-dip anchor.
        h.connect(0x40, SwarmNodeType::Storer);
        h.connect(0x41, SwarmNodeType::Storer);
        tokio::time::advance(Duration::from_secs(15)).await;
        let s = h.handle.readiness();
        assert_eq!(s.neighborhood_stable_for, Some(Duration::from_secs(15)));
        assert!(!s.is_neighborhood_ready());

        tokio::time::advance(Duration::from_secs(15)).await;
        assert!(h.handle.readiness().is_neighborhood_ready());
    }

    #[tokio::test(start_paused = true)]
    async fn neighborhood_stability_resets_on_depth_change() {
        let h = harness(SwarmNodeType::Storer, 64);
        saturate_to_depth_one(&h);
        tokio::time::advance(Duration::from_secs(20)).await;

        // Grow bin 2 to 7 peers: depth (anchored at unsaturated bin 1) and
        // saturation are unchanged, so the clock keeps running.
        for n in 0x23..0x27 {
            h.connect(n, SwarmNodeType::Storer);
        }
        let s = h.handle.readiness();
        assert_eq!(s.depth.get(), 1);
        assert_eq!(
            s.neighborhood_stable_for,
            Some(Duration::from_secs(20)),
            "mutations that move neither depth nor saturation keep the clock"
        );

        // Saturate bin 1 (5 -> 8): the unsaturated frontier moves to bin 2
        // and depth climbs. Bins at and above 2 hold 7 + 1 = 8 connected,
        // so the neighborhood is still saturated, but the boundary moved:
        // the clock must restart.
        for n in 0x45..0x48 {
            h.connect(n, SwarmNodeType::Storer);
        }
        let s = h.handle.readiness();
        assert_eq!(s.depth.get(), 2);
        assert!(s.is_saturated());
        assert_eq!(
            s.neighborhood_stable_for,
            Some(Duration::ZERO),
            "depth change must restart the stability clock"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn wait_until_neighborhood_ready_resolves_after_window() {
        let h = harness(SwarmNodeType::Storer, 64);

        let mut waiter = Box::pin(h.handle.wait_until_neighborhood_ready());
        assert!(futures::poll!(waiter.as_mut()).is_pending());

        saturate_to_depth_one(&h);
        h.emit_peer_ready(0x80, SwarmNodeType::Storer);
        // Saturation alone must not resolve the gate; the window has to pass.
        assert!(futures::poll!(waiter.as_mut()).is_pending());

        // Paused time auto-advances to the armed window timer once the
        // runtime is otherwise idle.
        tokio::time::timeout(Duration::from_secs(60), waiter)
            .await
            .expect("neighborhood gate must resolve once the window elapses")
            .expect("wait_until_neighborhood_ready must succeed");
    }

    #[tokio::test]
    async fn dropping_wait_future_releases_subscription() {
        let h = harness(SwarmNodeType::Client, 16);
        assert_eq!(h.event_tx.receiver_count(), 0);

        let mut waiter = Box::pin(h.handle.wait_until_routable());
        assert!(futures::poll!(waiter.as_mut()).is_pending());
        assert_eq!(h.event_tx.receiver_count(), 1);

        drop(waiter);
        assert_eq!(
            h.event_tx.receiver_count(),
            0,
            "dropping the future must drop its subscription"
        );
    }

    #[tokio::test]
    async fn lagged_receiver_reevaluates_state() {
        // Capacity 1: the second send overwrites the first and the parked
        // receiver observes Lagged instead of the events themselves.
        let h = harness(SwarmNodeType::Client, 1);

        let mut waiter = Box::pin(h.handle.wait_until_routable());
        assert!(futures::poll!(waiter.as_mut()).is_pending());

        // State flips while the receiver is parked, but the only events in
        // the stream are pings, which the waiter does not re-evaluate on.
        // Resolution therefore proves the Lagged arm re-read the state.
        h.connect(1, SwarmNodeType::Storer);
        h.emit_ping(1);
        h.emit_ping(1);

        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("lagged waiter must re-evaluate state")
            .expect("wait_until_routable must succeed");
    }

    #[tokio::test]
    async fn wait_until_custom_predicate_tracks_disconnects() {
        let h = harness(SwarmNodeType::Client, 16);
        h.connect(1, SwarmNodeType::Storer);
        h.connect(2, SwarmNodeType::Storer);

        // A predicate over the full snapshot resolves immediately when
        // already true.
        h.handle
            .wait_until(|s| s.connected_peers >= 2)
            .await
            .expect("predicate already true");

        let mut waiter = Box::pin(h.handle.wait_until(|s| s.connected_peers <= 1));
        assert!(futures::poll!(waiter.as_mut()).is_pending());

        let overlay = test_overlay(2);
        SwarmRouting::on_peer_disconnected(&*h.routing, &overlay);
        h.peer_manager.on_peer_disconnected(&overlay, "test");
        let _ = h.event_tx.send(TopologyEvent::PeerDisconnected {
            overlay,
            reason: crate::DisconnectReason::ConnectionError,
            connection_duration: None,
            node_type: SwarmNodeType::Storer,
        });

        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("predicate must re-evaluate on PeerDisconnected")
            .expect("wait_until must succeed");
    }
}
