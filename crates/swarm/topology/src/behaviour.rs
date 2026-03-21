//! Network topology behaviour managing peer connections via handshake, hive, and pingpong.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use tokio::sync::{broadcast, mpsc};
use tokio::time::Interval;

use libp2p::{
    Multiaddr, PeerId,
    core::{Endpoint, transport::PortUse},
    swarm::{
        ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, THandlerInEvent,
        THandlerOutEvent, ToSwarm,
    },
};
use tracing::{debug, info};
use vertex_net_local::{AddressScope, LocalCapabilities, same_subnet};
use vertex_net_peer_store::NetPeerStore;
use vertex_net_peer_store::error::StoreError;
use vertex_swarm_api::SwarmScoreStore;
use vertex_swarm_api::{PeerConfigValues, SwarmBootnodeConfig, SwarmIdentity};
use vertex_swarm_net_handshake::HANDSHAKE_TIMEOUT;
use vertex_swarm_net_hive::MAX_BATCH_SIZE;
use vertex_swarm_net_identify as identify;
use vertex_swarm_peer::SwarmPeer;
use vertex_swarm_peer_manager::{PeerManager, StoredPeer};
use vertex_swarm_peer_score::PeerScore;
use vertex_swarm_peer_score::SwarmScoringConfig;
use vertex_swarm_primitives::{OverlayAddress, SwarmNodeType};
use vertex_swarm_spec::HasSpec;

use crate::DialReason;
use vertex_net_dialer::{DialTracker, DialTrackerConfig};
use vertex_net_peer_registry::PeerRegistry;

pub(crate) type ConnectionRegistry = PeerRegistry<OverlayAddress, Option<DialReason>>;
use crate::TopologyCommand;
use crate::composed::ProtocolBehaviours;
use crate::error::TopologyError;
use crate::events::TopologyEvent;
use crate::extract_peer_id;
use crate::gossip::{GossipHandle, spawn_gossip_task};
use crate::handle::TopologyHandle;
use crate::kademlia::{KademliaConfig, KademliaRouting, RoutingEvaluatorHandle, SwarmRouting};
use crate::metrics::{TopologyMetrics, po_label};
use crate::nat_discovery::LocalAddressManager;

/// Type-erased peer store supporting both file-based and database-backed storage.
type PeerStore = Arc<dyn NetPeerStore<StoredPeer>>;

/// Default interval between connection evaluation rounds.
pub const DEFAULT_DIAL_INTERVAL: Duration = Duration::from_secs(5);

/// Post-handshake connections shorter than this are penalized as early disconnects.
const DEFAULT_EARLY_DISCONNECT_THRESHOLD: Duration = Duration::from_secs(30);

/// Default interval between periodic peer saves to persistent storage.
const DEFAULT_PEER_SAVE_INTERVAL: Duration = Duration::from_secs(300);

/// Event broadcast buffer (256 allows burst without blocking poll loop).
const EVENT_CHANNEL_CAPACITY: usize = 256;

/// Command buffer (64 is sufficient for typical dial/disconnect rate).
const COMMAND_CHANNEL_CAPACITY: usize = 64;

/// Target for dialing a peer (internal).
#[derive(Debug)]
pub(crate) enum DialTarget {
    /// Known peer from gossip/store - has overlay for verification during handshake.
    Known(SwarmPeer),

    /// Unknown peer - overlay learned at handshake.
    /// Multiaddr must contain /p2p/ component.
    Unknown(Multiaddr),
}

impl DialTarget {
    /// Get the PeerId for this dial target, if present in the multiaddr.
    pub(crate) fn peer_id(&self) -> Option<PeerId> {
        match self {
            Self::Known(peer) => peer.multiaddrs().iter().find_map(extract_peer_id),
            Self::Unknown(addr) => extract_peer_id(addr),
        }
    }

    /// Get the overlay address if known.
    pub(crate) fn overlay(&self) -> Option<OverlayAddress> {
        match self {
            Self::Known(peer) => Some(OverlayAddress::from(*peer.overlay())),
            Self::Unknown(_) => None,
        }
    }

    /// Get the addresses to dial.
    pub(crate) fn addrs(&self) -> Vec<Multiaddr> {
        match self {
            Self::Known(peer) => peer.multiaddrs().to_vec(),
            Self::Unknown(addr) => vec![addr.clone()],
        }
    }
}

/// Configuration for topology behaviour.
#[derive(Debug, Clone)]
pub struct TopologyConfig {
    pub kademlia: KademliaConfig,
    pub dial_interval: Duration,
    pub early_disconnect_threshold: Duration,
    pub peer_save_interval: Duration,
}

impl Default for TopologyConfig {
    fn default() -> Self {
        Self {
            kademlia: KademliaConfig::default(),
            dial_interval: DEFAULT_DIAL_INTERVAL,
            early_disconnect_threshold: DEFAULT_EARLY_DISCONNECT_THRESHOLD,
            peer_save_interval: DEFAULT_PEER_SAVE_INTERVAL,
        }
    }
}

impl TopologyConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_kademlia(mut self, config: KademliaConfig) -> Self {
        self.kademlia = config;
        self
    }

    pub fn with_dial_interval(mut self, interval: Duration) -> Self {
        self.dial_interval = interval;
        self
    }

    pub fn with_early_disconnect_threshold(mut self, threshold: Duration) -> Self {
        self.early_disconnect_threshold = threshold;
        self
    }

    pub fn with_peer_save_interval(mut self, interval: Duration) -> Self {
        self.peer_save_interval = interval;
        self
    }
}

/// Network topology behaviour managing peer connections.
///
/// Creates and owns all internal state (routing, peer_manager, dial_tracker, etc.)
/// and provides a [`TopologyHandle`] for external queries and commands.
///
/// Composes `HandshakeBehaviour`, `HiveBehaviour`, and `PingpongBehaviour` for
/// protocol handling, delegating to each while coordinating connection state.
pub struct TopologyBehaviour<I: SwarmIdentity + Clone> {
    pub(crate) identity: Arc<I>,

    /// Composed protocol behaviours (handshake, hive, pingpong).
    pub(crate) protocols: ProtocolBehaviours<I>,

    // Shared with TopologyHandle (Arc for external access)
    pub(crate) routing: Arc<KademliaRouting<I>>,
    pub(crate) peer_manager: Arc<PeerManager<I>>,

    // Owned (internal only, Arc for handler sharing and routing integration)
    pub(crate) connection_registry: Arc<ConnectionRegistry>,
    pub(crate) nat_discovery: Arc<LocalAddressManager>,
    pub(crate) bootnodes: Vec<Multiaddr>,
    pub(crate) trusted_peers: Vec<Multiaddr>,

    // Channels
    pub(crate) command_rx: mpsc::Receiver<TopologyCommand>,
    pub(crate) event_tx: broadcast::Sender<TopologyEvent>,

    // Pending swarm actions (dials, close connections, external addrs)
    pub(crate) pending_actions: VecDeque<ToSwarm<(), THandlerInEvent<ProtocolBehaviours<I>>>>,

    // Gossip coordination (async task with channel-based API)
    pub(crate) gossip: GossipHandle,

    // Periodic dial interval
    pub(crate) dial_interval: Pin<Box<Interval>>,

    // Periodic peer save interval (only ticks when peer_store is Some)
    pub(crate) peer_save_interval: Pin<Box<Interval>>,

    // Pending dnsaddr resolution for bootnodes (resolved_bootnodes, resolved_trusted)
    #[allow(clippy::type_complexity)]
    pub(crate) pending_bootnode_resolution:
        Option<Pin<Box<dyn Future<Output = (Vec<Multiaddr>, Vec<Multiaddr>)> + Send>>>,

    /// Static NAT addresses to emit as external addresses on first poll.
    /// Cleared after emitting to avoid re-emission.
    pub(crate) pending_nat_external_addrs: Vec<Multiaddr>,

    /// Handle for triggering background connection evaluation.
    pub(crate) evaluator_handle: RoutingEvaluatorHandle,

    /// Dial tracker for all outbound dials.
    /// Overlay may be unknown at dial time (bootnodes, commands).
    pub(crate) dial_tracker: DialTracker<OverlayAddress, DialReason>,

    /// Threshold for detecting post-handshake early disconnects.
    pub(crate) early_disconnect_threshold: Duration,

    /// Overlays pending eviction from bin trimming (consumed by handle_connection_closed).
    pub(crate) pending_evictions: HashSet<OverlayAddress>,

    /// Node type recorded at PeerReady time for symmetric metric decrement on disconnect.
    ///
    /// Without this, gossip re-verification can overwrite the handshake-confirmed
    /// node_type in PeerManager, causing the disconnect to decrement the wrong counter.
    pub(crate) connected_node_types: HashMap<OverlayAddress, SwarmNodeType>,

    /// Receiver for peer ban notifications from PeerManager.
    pub(crate) ban_rx: broadcast::Receiver<OverlayAddress>,

    /// Persistent peer store (None for ephemeral mode).
    pub(crate) peer_store: Option<PeerStore>,

    /// Agent versions received via identify, shared with identify behaviour.
    pub(crate) agent_versions: identify::AgentVersions,

    // Metrics
    pub(crate) metrics: Arc<TopologyMetrics>,
}

impl<I: SwarmIdentity + Clone> TopologyBehaviour<I> {
    // Constructor

    /// Create topology behaviour and handle.
    pub fn new(
        identity: I,
        config: TopologyConfig,
        network_config: &impl SwarmBootnodeConfig,
        peer_store: Option<Arc<dyn NetPeerStore<StoredPeer>>>,
        score_store: Option<Arc<dyn SwarmScoreStore<Score = PeerScore, Error = StoreError>>>,
    ) -> Result<(Self, TopologyHandle<I>), TopologyError>
    where
        I: HasSpec,
    {
        let bootnodes = network_config.bootnodes().to_vec();
        let trusted_peers = network_config.trusted_peers().to_vec();
        let nat_addrs = network_config.nat_addrs().to_vec();

        let (event_tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let (command_tx, command_rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);

        let peer_config = network_config.peers();
        let scoring_config = SwarmScoringConfig::builder()
            .ban_threshold(peer_config.ban_threshold())
            .warn_threshold(peer_config.warn_threshold())
            .build();
        let peer_manager = if let Some(ref store) = peer_store {
            PeerManager::with_store(
                &identity,
                store.clone(),
                score_store,
                scoring_config,
                peer_config.max_per_bin(),
            )
        } else {
            PeerManager::with_config(&identity, scoring_config, peer_config.max_per_bin())
        };

        let local_overlay = identity.overlay_address();

        let connection_registry = Arc::new(ConnectionRegistry::new());
        let agent_versions = identify::new_agent_versions();

        let ban_rx = peer_manager.subscribe_bans();

        let routing = KademliaRouting::new(
            identity.clone(),
            config.kademlia.clone(),
            peer_manager.clone(),
        );

        let local_capabilities = Arc::new(LocalCapabilities::new());

        // LocalAddressManager handles NAT address advertisement
        // Note: We no longer track peer-observed addresses - they contain
        // ephemeral NAT ports that only work for the specific peer connection.
        let nat_discovery = Arc::new(if !nat_addrs.is_empty() {
            info!(count = nat_addrs.len(), "NAT addresses configured");
            LocalAddressManager::new(local_capabilities.clone(), nat_addrs)
        } else {
            LocalAddressManager::disabled(local_capabilities.clone())
        });

        let identity = Arc::new(identity);

        // Create composed protocol behaviours
        let protocols = ProtocolBehaviours::new(identity.clone(), nat_discovery.clone());

        let metrics = Arc::new(TopologyMetrics::new());

        let handle = TopologyHandle::new(
            identity.clone(),
            routing.clone(),
            connection_registry.clone(),
            peer_manager.clone(),
            command_tx,
            event_tx.clone(),
            agent_versions.clone(),
            metrics.clone(),
        );

        let dial_interval = config.dial_interval;

        // Queue static NAT addresses to emit as external addresses on first poll
        let pending_nat_external_addrs = nat_discovery.nat_addrs().to_vec();

        let executor = vertex_tasks::TaskExecutor::try_current()
            .map_err(|e| TopologyError::TaskSpawn(e.to_string()))?;

        // Spawn background connection evaluator
        let evaluator_handle = crate::kademlia::spawn_evaluator(routing.clone(), &executor);

        // Spawn interface watcher for push-based subnet discovery.
        crate::tasks::spawn_interface_watcher(&executor);

        // Spawn the gossip task (merged peer exchange + verification).
        let spec = <I as HasSpec>::spec(&*identity).clone();
        let gossip = spawn_gossip_task(
            spec,
            local_overlay,
            peer_manager.clone(),
            connection_registry.clone(),
            evaluator_handle.clone(),
            local_capabilities.clone(),
            &executor,
        )
        .map_err(|e| TopologyError::TaskSpawn(e.to_string()))?;

        let behaviour = Self {
            identity,
            protocols,
            routing,
            peer_manager,
            connection_registry,
            nat_discovery,
            bootnodes,
            trusted_peers,
            command_rx,
            event_tx,
            pending_actions: VecDeque::new(),
            gossip,
            dial_interval: Box::pin(tokio::time::interval(dial_interval)),
            peer_save_interval: Box::pin(tokio::time::interval(config.peer_save_interval)),
            pending_bootnode_resolution: None,
            evaluator_handle,
            dial_tracker: DialTracker::new(DialTrackerConfig {
                max_pending: 0,     // not used as a queue, only for direct in-flight tracking
                max_in_flight: 256, // generous limit; routing capacity is the real gate
                pending_ttl: HANDSHAKE_TIMEOUT,
                in_flight_timeout: HANDSHAKE_TIMEOUT,
                cleanup_interval: Duration::from_secs(30),
                metrics_label: Some("topology"),
                ..Default::default()
            }),
            early_disconnect_threshold: config.early_disconnect_threshold,
            pending_evictions: HashSet::new(),
            connected_node_types: HashMap::new(),
            ban_rx,
            peer_store,
            agent_versions,
            pending_nat_external_addrs,
            metrics,
        };

        Ok((behaviour, handle))
    }

    // Public methods

    /// Set the local PeerId for address advertisement in handshakes.
    ///
    /// Must be called after the libp2p Swarm is built. All multiaddrs
    /// advertised to peers will include `/p2p/{peer_id}`.
    pub fn set_local_peer_id(&self, peer_id: PeerId) {
        self.nat_discovery.set_local_peer_id(peer_id);
    }

    /// Record an observed address reported by a peer.
    ///
    /// This is typically called with the `observed_addr` from libp2p identify.
    /// If the address is public, it updates our NAT discovery state to enable
    /// connections to other public peers.
    pub fn on_observed_addr(&self, addr: &Multiaddr) {
        self.nat_discovery.on_observed_addr(addr);
    }

    /// Shared agent version map, populated by identify and read by topology handle.
    pub fn agent_versions(&self) -> identify::AgentVersions {
        Arc::clone(&self.agent_versions)
    }

    /// Shared topology metrics (atomic counters for connected peers).
    pub fn metrics(&self) -> Arc<TopologyMetrics> {
        Arc::clone(&self.metrics)
    }

    /// Handle a topology command (dial, close connection, etc.).
    pub fn on_command(&mut self, command: TopologyCommand) {
        match command {
            TopologyCommand::ConnectBootnodes => {
                self.connect_bootnodes();
            }
            TopologyCommand::Dial(addr) => {
                self.dial(DialTarget::Unknown(addr), DialReason::Command);
            }
            TopologyCommand::CloseConnection(overlay) => {
                let Some(peer_id) = self.connection_registry.resolve_peer_id(&overlay) else {
                    tracing::warn!(%overlay, "Cannot close connection: peer not found");
                    return;
                };
                debug!(%overlay, %peer_id, "Close connection command");
                self.pending_actions.push_back(ToSwarm::CloseConnection {
                    peer_id,
                    connection: libp2p::swarm::CloseConnection::All,
                });
            }
            TopologyCommand::BanPeer { overlay, reason } => {
                self.peer_manager.ban(&overlay, reason);
                SwarmRouting::remove_peer(&*self.routing, &overlay);
                if let Some(peer_id) = self.connection_registry.resolve_peer_id(&overlay) {
                    debug!(%overlay, %peer_id, "Disconnecting banned peer via command");
                    self.pending_actions.push_back(ToSwarm::CloseConnection {
                        peer_id,
                        connection: libp2p::swarm::CloseConnection::All,
                    });
                }
                debug!(%overlay, "Banned peer via command");
            }
            TopologyCommand::SavePeers => {
                self.save_peers();
            }
        }
    }

    // Peer store

    /// Collect dirty hot peers and flush all pending writes to persistent storage.
    pub(crate) fn save_peers(&self) {
        if self.peer_store.is_some() {
            self.peer_manager.collect_dirty();
            self.peer_manager.flush_write_buffer();
            debug!(
                peers = self.peer_manager.index().len(),
                "Flushed peers to store"
            );
        }
    }

    pub(crate) fn broadcast_peers(&mut self, to: OverlayAddress, peers: Vec<SwarmPeer>) {
        let Some(state) = self.connection_registry.get(&to) else {
            tracing::warn!(%to, "Cannot broadcast: peer not found");
            return;
        };
        if let Some(connection_id) = state.connection_id() {
            let peer_id = state.peer_id();
            for chunk in peers.chunks(MAX_BATCH_SIZE) {
                self.protocols
                    .hive
                    .broadcast(peer_id, connection_id, chunk.to_vec());
            }
        }
    }

    // Routing

    /// Drain candidates from the background evaluator's per-bin queues and dial them.
    pub(crate) fn drain_candidate_queues(&mut self) {
        let candidates = self.routing.drain_candidates();
        if candidates.is_empty() {
            return;
        }

        let mut dialable = self.peer_manager.get_dialable_peers(&candidates);
        dialable.retain(|peer| self.can_advertise_to(peer));

        for swarm_peer in dialable {
            self.dial(DialTarget::Known(swarm_peer), DialReason::Discovery);
        }
    }

    /// Evict surplus peers from overpopulated bins after depth change.
    ///
    /// Emits `CloseConnection` for each evicted peer. Existing event handlers
    /// (`handle_connection_closed`) handle cleanup of routing capacity and state.
    pub(crate) fn trim_overpopulated_bins(&mut self) {
        let candidates = self.routing.eviction_candidates();
        if candidates.is_empty() {
            return;
        }

        let mut trimmed = 0;
        for candidate in &candidates {
            let reason = self
                .connection_registry
                .get(&candidate.overlay)
                .and_then(|s| *s.reason());
            if matches!(reason, Some(DialReason::Trusted)) {
                continue;
            }

            let Some(peer_id) = self.connection_registry.resolve_peer_id(&candidate.overlay) else {
                continue;
            };

            debug!(
                %peer_id,
                overlay = %candidate.overlay,
                bin = candidate.bin,
                phase = ?candidate.phase,
                "Evicting peer: bin overpopulated after depth change"
            );

            self.pending_evictions.insert(candidate.overlay);
            self.pending_actions.push_back(ToSwarm::CloseConnection {
                peer_id,
                connection: libp2p::swarm::CloseConnection::All,
            });
            trimmed += 1;
        }

        if trimmed > 0 {
            info!(
                trimmed,
                total_candidates = candidates.len(),
                "Trimmed overpopulated bins"
            );
        }
    }

    // Metrics and helpers

    pub(crate) fn emit_event(&self, event: TopologyEvent) {
        self.metrics.record_event(&event);
        let _ = self.event_tx.send(event);
    }

    /// Push routing gauges for a single bin and global totals.
    pub(crate) fn push_routing_gauges(&self, po: u8) {
        let po_str = po_label(po);
        let (connected, known) = self.routing.bin_peer_counts(po);
        let (dialing, handshaking, active) = self.routing.bin_phase_counts(po);

        metrics::gauge!("topology_bin_connected_peers", "po" => po_str).set(connected as f64);
        metrics::gauge!("topology_bin_known_peers", "po" => po_str).set(known as f64);
        metrics::gauge!("topology_bin_dialing", "po" => po_str).set(dialing as f64);
        metrics::gauge!("topology_bin_handshaking", "po" => po_str).set(handshaking as f64);
        metrics::gauge!("topology_bin_active", "po" => po_str).set(active as f64);
        metrics::gauge!("topology_bin_effective", "po" => po_str)
            .set((dialing + handshaking + active) as f64);
    }

    /// Push per-bin target/ceiling gauges and the global nominal gauge (called on depth change).
    pub(crate) fn push_bin_targets(&self) {
        let depth = self.routing.depth();
        let limits = self.routing.limits();
        let bin_count = self.routing.bin_sizes().len();

        for po in 0..bin_count {
            let po_str = po_label(po as u8);
            let target = limits.target(po as u8, depth);
            let target_val = if target == usize::MAX {
                -1.0
            } else {
                target as f64
            };
            let ceiling_val = limits.ceiling(po as u8, depth);
            let ceiling = if ceiling_val == usize::MAX {
                -1.0
            } else {
                ceiling_val as f64
            };

            metrics::gauge!("topology_bin_target_peers", "po" => po_str).set(target_val);
            metrics::gauge!("topology_bin_ceiling_peers", "po" => po_str).set(ceiling);
        }

        metrics::gauge!("topology_bin_nominal_peers").set(limits.nominal() as f64);
    }

    /// Get the proximity order for a peer relative to our overlay address.
    pub(crate) fn proximity(&self, peer: &OverlayAddress) -> u8 {
        self.identity.overlay_address().proximity(peer)
    }

    /// Check if we can advertise to a peer based on address scope.
    ///
    /// - Public peers: require public addresses (NAT or discovered)
    /// - Private peers on LAN: can use private addresses if on same subnet
    /// - Loopback peers: always dialable
    pub(crate) fn can_advertise_to(&self, peer: &SwarmPeer) -> bool {
        let peer_max_scope = peer.max_scope();

        match peer_max_scope {
            Some(AddressScope::Public) => {
                // Public peer - need public addresses
                self.nat_discovery.has_public_addresses()
            }
            Some(AddressScope::Private | AddressScope::LinkLocal) => {
                // Private/link-local peer - check if we share a subnet
                let listen_addrs = self.nat_discovery.listen_addrs();
                peer.multiaddrs().iter().any(|peer_addr| {
                    listen_addrs
                        .iter()
                        .any(|our_addr| same_subnet(our_addr, peer_addr))
                })
            }
            Some(AddressScope::Loopback) | None => {
                // Loopback or unknown - allow
                true
            }
        }
    }
}

impl<I: SwarmIdentity + Clone + 'static> NetworkBehaviour for TopologyBehaviour<I> {
    type ConnectionHandler = <ProtocolBehaviours<I> as NetworkBehaviour>::ConnectionHandler;
    type ToSwarm = ();

    fn handle_established_inbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: PeerId,
        local_addr: &Multiaddr,
        remote_addr: &Multiaddr,
    ) -> Result<Self::ConnectionHandler, ConnectionDenied> {
        self.protocols.handle_established_inbound_connection(
            connection_id,
            peer,
            local_addr,
            remote_addr,
        )
    }

    fn handle_established_outbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: PeerId,
        addr: &Multiaddr,
        role_override: Endpoint,
        port_use: PortUse,
    ) -> Result<Self::ConnectionHandler, ConnectionDenied> {
        self.protocols.handle_established_outbound_connection(
            connection_id,
            peer,
            addr,
            role_override,
            port_use,
        )
    }

    fn on_swarm_event(&mut self, event: FromSwarm) {
        // Forward swarm events to composed protocols
        self.protocols.on_swarm_event(event);

        match event {
            FromSwarm::ConnectionEstablished(established) => {
                self.handle_connection_established(established);
            }
            FromSwarm::ConnectionClosed(closed) => {
                self.handle_connection_closed(closed);
            }
            FromSwarm::DialFailure(failure) => {
                self.handle_dial_failure(failure);
            }
            FromSwarm::NewListenAddr(info) => {
                debug!(address = %info.addr, "New listen address");
                let capability_became_known =
                    self.nat_discovery.on_new_listen_addr(info.addr.clone());

                if capability_became_known {
                    debug!("Network capability now known, triggering immediate dial");
                    self.evaluator_handle.trigger_evaluation();
                }
            }
            FromSwarm::ExpiredListenAddr(info) => {
                debug!(address = %info.addr, "Expired listen address");
                self.nat_discovery.on_expired_listen_addr(info.addr);
            }
            _ => {}
        }
    }

    fn on_connection_handler_event(
        &mut self,
        peer_id: PeerId,
        connection_id: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        // Forward to composed protocols
        self.protocols
            .on_connection_handler_event(peer_id, connection_id, event);
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        // Use TimingGuard for automatic poll duration recording
        let _poll_timer = vertex_observability::TimingGuard::new(metrics::histogram!(
            "topology_poll_duration_seconds"
        ));

        // Emit any pending static NAT addresses as external addresses (one-time on startup)
        if !self.pending_nat_external_addrs.is_empty() {
            for addr in std::mem::take(&mut self.pending_nat_external_addrs) {
                debug!(addr = %addr, "Emitting static NAT address as external address");
                self.pending_actions
                    .push_back(ToSwarm::ExternalAddrConfirmed(addr));
            }
        }

        // Poll for commands from TopologyHandle
        while let Poll::Ready(Some(command)) = self.command_rx.poll_recv(cx) {
            self.on_command(command);
        }

        // Poll pending dnsaddr resolution for bootnodes
        if let Some(ref mut future) = self.pending_bootnode_resolution
            && let Poll::Ready((resolved_bootnodes, resolved_trusted)) = future.as_mut().poll(cx)
        {
            info!(
                bootnodes = resolved_bootnodes.len(),
                trusted = resolved_trusted.len(),
                "dnsaddr resolution complete, dialing bootnodes"
            );
            self.pending_bootnode_resolution = None;
            self.dial_bootnodes(resolved_bootnodes, resolved_trusted);
        }

        // Drain gossip broadcast actions from the async gossip task
        while let Ok(action) = self.gossip.try_recv() {
            self.broadcast_peers(action.to, action.peers);
        }

        // Check for periodic dial candidate evaluation
        if self.dial_interval.as_mut().poll_tick(cx).is_ready() {
            self.cleanup_stale_pending();
            self.evaluator_handle.trigger_evaluation();
        }

        // Periodic peer save (safety net against crashes)
        if self.peer_store.is_some() && self.peer_save_interval.as_mut().poll_tick(cx).is_ready() {
            self.save_peers();
        }

        // Poll composed protocols and process their events
        loop {
            match self.protocols.poll(cx) {
                Poll::Ready(ToSwarm::GenerateEvent(event)) => {
                    metrics::counter!("topology_poll_events_total").increment(1);
                    let (peer_id, connection_id) = event.peer_connection();
                    self.process_protocol_event(peer_id, connection_id, event);
                }
                Poll::Ready(ToSwarm::Dial { opts }) => {
                    return Poll::Ready(ToSwarm::Dial { opts });
                }
                Poll::Ready(ToSwarm::NotifyHandler {
                    peer_id,
                    handler,
                    event,
                }) => {
                    return Poll::Ready(ToSwarm::NotifyHandler {
                        peer_id,
                        handler,
                        event,
                    });
                }
                Poll::Ready(ToSwarm::CloseConnection {
                    peer_id,
                    connection,
                }) => {
                    return Poll::Ready(ToSwarm::CloseConnection {
                        peer_id,
                        connection,
                    });
                }
                Poll::Ready(ToSwarm::ExternalAddrConfirmed(addr)) => {
                    return Poll::Ready(ToSwarm::ExternalAddrConfirmed(addr));
                }
                Poll::Ready(ToSwarm::ExternalAddrExpired(addr)) => {
                    return Poll::Ready(ToSwarm::ExternalAddrExpired(addr));
                }
                Poll::Ready(ToSwarm::NewExternalAddrCandidate(addr)) => {
                    return Poll::Ready(ToSwarm::NewExternalAddrCandidate(addr));
                }
                Poll::Ready(ToSwarm::ListenOn { opts }) => {
                    return Poll::Ready(ToSwarm::ListenOn { opts });
                }
                Poll::Ready(ToSwarm::RemoveListener { id }) => {
                    return Poll::Ready(ToSwarm::RemoveListener { id });
                }
                Poll::Ready(ToSwarm::NewExternalAddrOfPeer { peer_id, address }) => {
                    return Poll::Ready(ToSwarm::NewExternalAddrOfPeer { peer_id, address });
                }
                Poll::Ready(_) => {}
                Poll::Pending => break,
            }
        }

        // Drain candidates produced by the background evaluator task.
        self.drain_candidate_queues();

        // Drain ban notifications and disconnect banned peers.
        // Catches auto-bans from scoring events processed above.
        while let Ok(overlay) = self.ban_rx.try_recv() {
            if let Some(peer_id) = self.connection_registry.resolve_peer_id(&overlay) {
                debug!(%overlay, %peer_id, "Disconnecting auto-banned peer");
                SwarmRouting::remove_peer(&*self.routing, &overlay);
                self.pending_actions.push_back(ToSwarm::CloseConnection {
                    peer_id,
                    connection: libp2p::swarm::CloseConnection::All,
                });
            }
        }

        if let Some(action) = self.pending_actions.pop_front() {
            return Poll::Ready(action);
        }

        Poll::Pending
    }
}

impl<I: SwarmIdentity + Clone> Drop for TopologyBehaviour<I> {
    fn drop(&mut self) {
        // Collect dirty hot peers and flush pending writes on shutdown
        if self.peer_store.is_some() {
            self.peer_manager.collect_dirty();
            self.peer_manager.flush_write_buffer();
        }

        let active = self.connection_registry.active_count();
        let pending = self.connection_registry.pending_count();
        let depth = self.routing.depth();

        info!(
            active_peers = active,
            pending_connections = pending,
            depth,
            "Topology behaviour shutting down"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::kademlia::KademliaConfig;

    #[test]
    fn test_topology_config() {
        let config = TopologyConfig::new()
            .with_kademlia(KademliaConfig::default().with_nominal(3))
            .with_dial_interval(Duration::from_secs(10));

        assert_eq!(config.dial_interval, Duration::from_secs(10));
        assert_eq!(config.kademlia.limits.nominal(), 3);
    }
}
