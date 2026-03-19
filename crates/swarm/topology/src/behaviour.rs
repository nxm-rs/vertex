//! Network topology behaviour managing peer connections via handshake, hive, and pingpong.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use rand::seq::SliceRandom;
use tokio::sync::{broadcast, mpsc};
use tokio::time::Interval;

use libp2p::{
    Multiaddr, PeerId,
    core::{Endpoint, transport::PortUse},
    swarm::{
        ConnectionDenied, ConnectionError, ConnectionId, FromSwarm, NetworkBehaviour,
        THandlerInEvent, THandlerOutEvent, ToSwarm,
    },
};
use metrics::gauge;
use tracing::{debug, info, trace, warn};
use vertex_swarm_net_handshake::{HandshakeEvent, HANDSHAKE_TIMEOUT};
use vertex_swarm_net_hive::{HiveEvent, MAX_BATCH_SIZE};
use vertex_swarm_net_identify as identify;
use vertex_swarm_net_pingpong::PingpongEvent;
use vertex_net_local::{AddressScope, LocalCapabilities, same_subnet};
use vertex_swarm_api::{PeerConfigValues, SwarmBootnodeConfig, SwarmIdentity};
use vertex_swarm_peer::SwarmPeer;
use vertex_swarm_spec::HasSpec;
use vertex_net_peer_store::{NetPeerStore, StoreError};
use vertex_swarm_peer_score::PeerScore;
use vertex_swarm_api::SwarmScoreStore;
use vertex_swarm_peer_manager::{PeerManager, StoredPeer};
use vertex_swarm_peer_score::SwarmScoringConfig;
use vertex_swarm_primitives::{OverlayAddress, SwarmNodeType};

use vertex_net_dialer::{DialTracker, DialTrackerConfig, PrepareError};
use vertex_net_peer_registry::{ActivateResult, ConnectionState, PeerRegistry};
use crate::DialReason;

pub(crate) type ConnectionRegistry = PeerRegistry<OverlayAddress, Option<DialReason>>;
use crate::extract_peer_id;
use crate::composed::{ProtocolBehaviours, ProtocolEvent};
use vertex_net_dnsaddr::{is_dnsaddr, resolve_all};
use crate::error::{DialError, DisconnectReason, RejectionReason, TopologyError};
use crate::events::{ConnectionDirection, TopologyEvent};
use crate::gossip::{GossipHandle, GossipInput, spawn_gossip_task};
use crate::handle::TopologyHandle;
use crate::metrics::{TopologyMetrics, po_label};
use crate::nat_discovery::LocalAddressManager;
use crate::kademlia::{KademliaConfig, KademliaRouting, RoutingCapacity, RoutingEvaluatorHandle, SwarmRouting};
use crate::TopologyCommand;

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
    identity: Arc<I>,

    /// Composed protocol behaviours (handshake, hive, pingpong).
    protocols: ProtocolBehaviours<I>,

    // Shared with TopologyHandle (Arc for external access)
    routing: Arc<KademliaRouting<I>>,
    peer_manager: Arc<PeerManager<I>>,

    // Owned (internal only, Arc for handler sharing and routing integration)
    connection_registry: Arc<ConnectionRegistry>,
    nat_discovery: Arc<LocalAddressManager>,
    bootnodes: Vec<Multiaddr>,
    trusted_peers: Vec<Multiaddr>,

    // Channels
    command_rx: mpsc::Receiver<TopologyCommand>,
    event_tx: broadcast::Sender<TopologyEvent>,

    // Pending swarm actions (dials, close connections, external addrs)
    pending_actions: VecDeque<ToSwarm<(), THandlerInEvent<ProtocolBehaviours<I>>>>,

    // Gossip coordination (async task with channel-based API)
    gossip: GossipHandle,

    // Periodic dial interval
    dial_interval: Pin<Box<Interval>>,

    // Periodic peer save interval (only ticks when peer_store is Some)
    peer_save_interval: Pin<Box<Interval>>,

    // Pending dnsaddr resolution for bootnodes (resolved_bootnodes, resolved_trusted)
    pending_bootnode_resolution: Option<Pin<Box<dyn Future<Output = (Vec<Multiaddr>, Vec<Multiaddr>)> + Send>>>,

    /// Static NAT addresses to emit as external addresses on first poll.
    /// Cleared after emitting to avoid re-emission.
    pending_nat_external_addrs: Vec<Multiaddr>,

    /// Handle for triggering background connection evaluation.
    evaluator_handle: RoutingEvaluatorHandle,

    /// Dial tracker for all outbound dials.
    /// Overlay may be unknown at dial time (bootnodes, commands).
    dial_tracker: DialTracker<OverlayAddress, DialReason>,

    /// Threshold for detecting post-handshake early disconnects.
    early_disconnect_threshold: Duration,

    /// Overlays pending eviction from bin trimming (consumed by handle_connection_closed).
    pending_evictions: HashSet<OverlayAddress>,

    /// Node type recorded at PeerReady time for symmetric metric decrement on disconnect.
    ///
    /// Without this, gossip re-verification can overwrite the handshake-confirmed
    /// node_type in PeerManager, causing the disconnect to decrement the wrong counter.
    connected_node_types: HashMap<OverlayAddress, SwarmNodeType>,

    /// Receiver for peer ban notifications from PeerManager.
    ban_rx: broadcast::Receiver<OverlayAddress>,

    /// Persistent peer store (None for ephemeral mode).
    peer_store: Option<PeerStore>,

    /// Agent versions received via identify, shared with identify behaviour.
    agent_versions: identify::AgentVersions,

    // Metrics
    metrics: Arc<TopologyMetrics>,
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
            PeerManager::with_config(
                &identity,
                scoring_config,
                peer_config.max_per_bin(),
            )
        };

        let local_overlay = identity.overlay_address();

        let connection_registry = Arc::new(ConnectionRegistry::new());
        let agent_versions = identify::new_agent_versions();

        let ban_rx = peer_manager.subscribe_bans();

        let routing = KademliaRouting::new(identity.clone(), config.kademlia.clone(), peer_manager.clone());

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
        let protocols = ProtocolBehaviours::new(
            identity.clone(),
            nat_discovery.clone(),
        );

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

        // Spawn background connection evaluator
        let evaluator_handle = routing
            .spawn_evaluator()
            .map_err(|e| TopologyError::VerifierSpawn(e))?;

        // Spawn interface watcher for push-based subnet discovery.
        // if-watch subscribes to netlink address events and fires initial Up
        // events for all existing addresses, then ongoing Up/Down as interfaces change.
        {
            let executor = vertex_tasks::TaskExecutor::try_current()
                .map_err(|e| TopologyError::VerifierSpawn(e.to_string()))?;
            executor.spawn_with_graceful_shutdown_signal(
                "net.interface_watcher",
                move |shutdown| async move {
                    use futures::StreamExt;

                    let mut watcher = match if_watch::tokio::IfWatcher::new() {
                        Ok(w) => w,
                        Err(e) => {
                            tracing::error!(error = %e, "failed to create interface watcher");
                            return;
                        }
                    };

                    let mut shutdown = std::pin::pin!(shutdown);
                    loop {
                        tokio::select! {
                            guard = &mut shutdown => {
                                drop(guard);
                                break;
                            }
                            event = watcher.next() => {
                                match event {
                                    Some(Ok(if_watch::IfEvent::Up(net))) => {
                                        vertex_net_local::add_subnet(net);
                                    }
                                    Some(Ok(if_watch::IfEvent::Down(net))) => {
                                        vertex_net_local::remove_subnet(net);
                                    }
                                    Some(Err(e)) => {
                                        tracing::warn!(error = %e, "interface watcher error");
                                    }
                                    None => break,
                                }
                            }
                        }
                    }
                },
            );
        }

        // Spawn the gossip task (merged peer exchange + verification).
        let spec = <I as HasSpec>::spec(&*identity).clone();
        let gossip = spawn_gossip_task(
            spec,
            local_overlay,
            peer_manager.clone(),
            connection_registry.clone(),
            evaluator_handle.clone(),
            local_capabilities.clone(),
        )
        .map_err(|e| TopologyError::VerifierSpawn(e.to_string()))?;

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

    /// Dial a known SwarmPeer for discovery.
    ///
    /// Checks routing capacity and filters before dialing.
    pub fn dial_swarm_peer(&mut self, swarm_peer: SwarmPeer) -> bool {
        let overlay = OverlayAddress::from(*swarm_peer.overlay());

        // Check if banned or in backoff
        if self.peer_manager.is_banned(&overlay) || self.peer_manager.peer_is_in_backoff(&overlay) {
            return false;
        }

        // Check scope compatibility
        if !self.can_advertise_to(&swarm_peer) {
            return false;
        }

        self.dial(DialTarget::Known(swarm_peer), DialReason::Discovery);
        true
    }

    /// Process a batch of dial requests.
    ///
    /// Returns the number of dials that were successfully initiated.
    pub fn dial_batch(&mut self, peers: impl IntoIterator<Item = SwarmPeer>) -> usize {
        let mut dialed = 0;
        for peer in peers {
            if self.dial_swarm_peer(peer) {
                dialed += 1;
            }
        }
        dialed
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
                    warn!(%overlay, "Cannot close connection: peer not found");
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

    // Connection management

    /// Dial a peer target.
    ///
    /// For Known peers: checks routing capacity, registers in DialTracker, verifies during handshake.
    /// For Unknown peers: no capacity check, tracked in pending_unknown_dials, learns overlay at handshake.
    fn dial(&mut self, target: DialTarget, reason: DialReason) {
        let Some(peer_id) = target.peer_id() else {
            warn!(?target, "Cannot dial: no /p2p/ component in address");
            return;
        };

        if self.is_peer_tracked(&peer_id) {
            trace!(%peer_id, "Skipping dial - already tracked");
            return;
        }

        // For Known peers, check routing capacity before dialing
        if let Some(overlay) = target.overlay() {
            if !self.routing.try_reserve_dial(&overlay, SwarmNodeType::Storer) {
                trace!(%overlay, "Skipping dial - at capacity or already tracking");
                return;
            }
        }

        // One call: filter addresses, build DialOpts, register in-flight
        let capability = self.nat_discovery.capability();
        let opts = match self.dial_tracker.prepare_and_start(
            target.overlay(),
            peer_id,
            target.addrs(),
            reason,
            |addr| vertex_net_local::is_dialable(addr, capability),
        ) {
            Ok(opts) => opts,
            Err(PrepareError::NoReachableAddresses) => {
                if let Some(overlay) = target.overlay() {
                    self.routing.release_dial(&overlay);
                    self.peer_manager.record_dial_failure(&overlay);
                }
                debug!(%peer_id, ?capability, "No reachable addresses");
                return;
            }
            Err(PrepareError::AlreadyTracked) => {
                if let Some(overlay) = target.overlay() {
                    self.routing.release_dial(&overlay);
                }
                trace!(%peer_id, "Skipping dial - already in dial tracker");
                return;
            }
            Err(PrepareError::InBackoff | PrepareError::Banned) => {
                if let Some(overlay) = target.overlay() {
                    self.routing.release_dial(&overlay);
                }
                trace!(%peer_id, "Skipping dial - peer in backoff or banned");
                return;
            }
        };

        debug!(%peer_id, ?reason, "Dialing peer");

        // Track discovery dials for delayed gossip exchange
        if reason == DialReason::Discovery {
            self.gossip.send(GossipInput::MarkGossipDial(peer_id));
        }

        self.pending_actions.push_back(ToSwarm::Dial { opts });
    }

    fn connect_bootnodes(&mut self) {
        let mut bootnodes = self.bootnodes.clone();
        bootnodes.shuffle(&mut rand::rng());
        let trusted_peers = self.trusted_peers.clone();

        if bootnodes.is_empty() && trusted_peers.is_empty() {
            return;
        }

        // Check if any addresses need dnsaddr resolution
        let needs_resolution = bootnodes.iter().any(|addr| is_dnsaddr(addr))
            || trusted_peers.iter().any(|addr| is_dnsaddr(addr));

        if needs_resolution {
            info!(
                bootnodes = bootnodes.len(),
                trusted = trusted_peers.len(),
                "Resolving dnsaddr entries for bootnodes..."
            );

            // Resolve bootnodes and trusted peers separately to preserve DialReason
            let future = Box::pin(async move {
                let resolved_bootnodes = resolve_all(bootnodes.iter()).await;
                let resolved_trusted = resolve_all(trusted_peers.iter()).await;
                (resolved_bootnodes, resolved_trusted)
            });
            self.pending_bootnode_resolution = Some(future);
        } else {
            // No resolution needed, dial immediately
            self.dial_bootnodes(bootnodes, trusted_peers);
        }
    }

    /// Dial bootnodes and trusted peers (called after dnsaddr resolution if needed).
    fn dial_bootnodes(&mut self, bootnodes: Vec<Multiaddr>, trusted_peers: Vec<Multiaddr>) {
        if !bootnodes.is_empty() {
            info!(count = bootnodes.len(), "Connecting to all bootnodes...");
        }

        for addr in bootnodes {
            self.dial(DialTarget::Unknown(addr), DialReason::Bootnode);
        }

        for addr in trusted_peers {
            self.dial(DialTarget::Unknown(addr), DialReason::Trusted);
        }
    }

    /// Check if a PeerId is already being tracked (dialing, connected, or active).
    fn is_peer_tracked(&self, peer_id: &PeerId) -> bool {
        self.connection_registry.contains_peer(peer_id)
            || self.dial_tracker.contains_peer(peer_id)
    }

    // Event handlers

    #[tracing::instrument(skip_all, level = "trace", fields(%peer_id))]
    fn process_protocol_event(
        &mut self,
        peer_id: PeerId,
        connection_id: ConnectionId,
        event: ProtocolEvent,
    ) {
        match event {
            ProtocolEvent::Handshake(HandshakeEvent::Completed { info, .. }) => {
                self.on_handshake_completed(peer_id, connection_id, *info);
            }
            ProtocolEvent::Handshake(HandshakeEvent::Failed { error, .. }) => {
                self.on_handshake_failed(peer_id, error);
            }
            ProtocolEvent::Hive(HiveEvent::PeersReceived { peers, .. }) => {
                self.on_hive_peers_received(peer_id, peers);
            }
            ProtocolEvent::Hive(HiveEvent::Error { error, .. }) => {
                warn!(%peer_id, %error, "Hive error");
            }
            ProtocolEvent::Pingpong(PingpongEvent::Pong { rtt, .. }) => {
                self.on_pingpong_pong(peer_id, rtt);
            }
            ProtocolEvent::Pingpong(PingpongEvent::PingReceived { .. }) => {
                debug!(%peer_id, "Received ping from peer");
            }
            ProtocolEvent::Pingpong(PingpongEvent::Error { error, .. }) => {
                warn!(%peer_id, %error, "Pingpong failed");
            }
        }
    }

    #[tracing::instrument(skip(self, info), level = "debug", fields(%peer_id))]
    fn on_handshake_completed(
        &mut self,
        peer_id: PeerId,
        connection_id: ConnectionId,
        info: vertex_swarm_net_handshake::HandshakeInfo,
    ) {
        let overlay = OverlayAddress::from(*info.swarm_peer.overlay());
        let node_type = info.node_type;

        debug!(
            %peer_id,
            %overlay,
            ?node_type,
            po = self.proximity(&overlay),
            "Handshake completed"
        );

        // Get dial info from connection registry before transitioning
        let current_state = self.connection_registry.get(&overlay)
            .or_else(|| self.connection_registry.resolve_id(&peer_id)
                .and_then(|o| self.connection_registry.get(&o)));
        let direction = current_state.as_ref()
            .and_then(|s| s.direction())
            .unwrap_or(ConnectionDirection::Inbound);

        // Reject banned peers immediately (inbound peers bypass dial-time ban check).
        if self.peer_manager.is_banned(&overlay) {
            debug!(
                %peer_id,
                %overlay,
                ?direction,
                "Rejecting connection: peer is banned"
            );
            self.emit_event(TopologyEvent::PeerRejected {
                overlay,
                peer_id,
                reason: RejectionReason::Banned,
                direction,
            });
            self.pending_actions.push_back(ToSwarm::CloseConnection {
                peer_id,
                connection: libp2p::swarm::CloseConnection::All,
            });
            return;
        }

        // For inbound connections, check bin capacity and reserve a slot before
        // transitioning to active. Outbound connections already reserved capacity
        // at dial time via try_reserve_dial.
        if direction == ConnectionDirection::Inbound {
            let bin_at_capacity = !RoutingCapacity::should_accept_inbound(&*self.routing, &overlay, node_type);
            if bin_at_capacity {
                debug!(
                    %peer_id,
                    %overlay,
                    ?node_type,
                    ?direction,
                    "Rejecting inbound connection: bin saturated"
                );
                self.emit_event(TopologyEvent::PeerRejected {
                    overlay,
                    peer_id,
                    reason: RejectionReason::BinSaturated,
                    direction,
                });
                self.pending_actions.push_back(ToSwarm::CloseConnection {
                    peer_id,
                    connection: libp2p::swarm::CloseConnection::All,
                });
                return;
            }
            // Reserve inbound slot so handshake_completed can transition Handshaking→Active
            RoutingCapacity::reserve_inbound(&*self.routing, &overlay);
        }

        // Transition to Active state in connection registry
        let activate_result = self.connection_registry.activate(
            peer_id,
            connection_id,
            overlay,
        );
        match &activate_result {
            ActivateResult::Accepted => {
                gauge!("peer_registry_pending_connections").decrement(1.0);
                gauge!("peer_registry_active_connections").increment(1.0);
            }
            ActivateResult::Replaced { old_id: None, .. } => {
                gauge!("peer_registry_pending_connections").decrement(1.0);
            }
            ActivateResult::Replaced { old_id: Some(_), .. } => {}
        }

        // Update routing capacity tracking (transitions Handshaking→Active)
        RoutingCapacity::handshake_completed(&*self.routing, &overlay);

        // Handle the activate result from connection registry
        match activate_result {
            ActivateResult::Replaced { old_peer_id, old_connection_id, ref old_id } => {
                // The old connection was already counted by a prior PeerReady event.
                // Its registry entry is now overwritten, so handle_connection_closed
                // will not emit PeerDisconnected — we must decrement here.
                // Use connected_node_types (recorded at PeerReady time) for symmetric decrement.
                let old_overlay = old_id.as_ref().unwrap_or(&overlay);
                let old_node_type = self.connected_node_types.remove(old_overlay)
                    .unwrap_or(SwarmNodeType::Client);
                self.metrics.decrement_connected(old_node_type);
                gauge!("peer_registry_active_connections").decrement(1.0);

                debug!(
                    %peer_id,
                    %old_peer_id,
                    ?old_connection_id,
                    %overlay,
                    "Closing old connection, new connection takes over"
                );
                self.emit_event(TopologyEvent::PeerRejected {
                    overlay,
                    peer_id: old_peer_id,
                    reason: RejectionReason::DuplicateConnection,
                    direction,
                });
                // Close only the specific old connection, not all connections.
                // This handles racing dialers (same PeerId claiming same overlay)
                // correctly by keeping the new connection active.
                self.pending_actions.push_back(ToSwarm::CloseConnection {
                    peer_id: old_peer_id,
                    connection: libp2p::swarm::CloseConnection::One(old_connection_id),
                });
            }
            ActivateResult::Accepted => {}
        }

        // Store peer metadata
        self.peer_manager.on_peer_ready(info.swarm_peer.clone(), info.node_type);

        let po = self.proximity(&overlay);

        let old_depth = self.routing.depth();
        self.routing.connected(overlay);
        let new_depth = self.routing.depth();

        // Push event-driven routing gauges for the affected bin
        self.push_routing_gauges(po);

        if new_depth != old_depth {
            self.push_bin_targets();
            self.gossip.send(GossipInput::DepthChanged(new_depth));
            self.emit_event(TopologyEvent::DepthChanged {
                old_depth,
                new_depth,
            });
            if new_depth > old_depth {
                self.trim_overpopulated_bins();
            }
        }

        // Record node_type for symmetric decrement on disconnect.
        self.connected_node_types.insert(overlay, node_type);

        self.emit_event(TopologyEvent::PeerReady {
            overlay,
            peer_id,
            node_type,
            direction,
        });

        // Notify gossip task — exchange happens immediately or after delay (for gossip dials)
        self.gossip.send(GossipInput::PeerActivated {
            peer_id,
            swarm_peer: info.swarm_peer,
            node_type,
        });

        // Dial completed successfully - coalesced evaluation in poll()
        self.evaluator_handle.trigger_evaluation();
    }

    fn on_handshake_failed(
        &mut self,
        peer_id: PeerId,
        error: vertex_swarm_net_handshake::HandshakeError,
    ) {
        warn!(%peer_id, %error, "Handshake failed");

        // Handshake failed means the peer was already registered in connection_registry.
        // Remove it and release routing capacity.
        let state = self.connection_registry.disconnected(&peer_id);
        if let Some(ref s) = state {
            decrement_connection_phase_gauge(s);
        }
        let reason = state.as_ref().and_then(|s| *s.reason());
        let overlay = state.as_ref().and_then(|s| s.id());

        if let Some(ref overlay) = overlay {
            self.routing.release_handshake(overlay);
            self.peer_manager.record_dial_failure(overlay);
        }

        self.emit_event(TopologyEvent::DialFailed {
            overlay,
            addrs: Vec::new(),
            error: DialError::HandshakeFailed(error.to_string()),
            dial_duration: state.as_ref().and_then(|s| s.started_at()).map(|t| t.elapsed()),
            reason,
        });
    }

    fn on_hive_peers_received(&mut self, peer_id: PeerId, peers: Vec<vertex_swarm_peer::SwarmPeer>) {
        if peers.is_empty() {
            return;
        }

        // Filter peers we can't reach based on our IP capability.
        let local_capability = self.nat_discovery.capability();
        let peers: Vec<vertex_swarm_peer::SwarmPeer> = if local_capability.is_known() {
            peers
                .into_iter()
                .filter(|peer| {
                    let peer_cap = peer.ip_capability();
                    let reachable = local_capability.can_reach(&peer_cap);
                    if !reachable {
                        trace!(
                            overlay = %peer.overlay(),
                            ?local_capability,
                            ?peer_cap,
                            "filtering unreachable gossiped peer"
                        );
                    }
                    reachable
                })
                .collect()
        } else {
            // Capability unknown (no listen addrs yet) — let all through
            peers
        };

        if peers.is_empty() {
            return;
        }

        let gossiper = self
            .connection_registry
            .resolve_id(&peer_id)
            .unwrap_or_else(|| {
                warn!(%peer_id, "Hive peers from unknown peer");
                OverlayAddress::default()
            });

        let peer_count = peers.len();
        self.gossip.send(GossipInput::PeersReceived { gossiper, peers });

        // Disconnect from bootnodes after receiving the initial peer list.
        // Bootnodes are gossip amplifiers — every new peer connecting to the bootnode
        // triggers a hive stream to all existing connections. Staying connected produces
        // a flood of 1-peer hive messages (~2/s on mainnet) that overwhelms rate limiters.
        let reason = self.connection_registry.get(&gossiper)
            .and_then(|s| *s.reason());
        if reason == Some(DialReason::Bootnode) {
            info!(
                %peer_id,
                %gossiper,
                peer_count,
                "Disconnecting from bootnode after initial hive gossip"
            );
            self.pending_actions.push_back(ToSwarm::CloseConnection {
                peer_id,
                connection: libp2p::swarm::CloseConnection::All,
            });
        }
    }

    fn on_pingpong_pong(&mut self, peer_id: PeerId, rtt: Duration) {
        debug!(%peer_id, ?rtt, "Pingpong success");

        if let Some(overlay) = self.connection_registry.resolve_id(&peer_id) {
            self.peer_manager.record_latency(&overlay, rtt);
            debug!(%peer_id, %overlay, ?rtt, "Connection health verified");

            self.emit_event(TopologyEvent::PingCompleted { overlay, rtt });
        }
    }

    // Lifecycle

    fn handle_connection_established(&mut self, established: libp2p::swarm::behaviour::ConnectionEstablished) {
        if established.endpoint.is_dialer() {
            // Resolve from DialTracker (sole source of outbound dial tracking)
            if let Some(request) = self.dial_tracker.resolve(&established.peer_id) {
                let overlay = request.id;
                let reason = request.data;
                let result = self.connection_registry.connected_outbound(
                    established.peer_id,
                    established.connection_id,
                    overlay.clone(),
                    request.queued_at(),
                    Some(reason),
                );
                if result.is_some() {
                    gauge!("peer_registry_pending_connections").increment(1.0);
                }
                if let Some(overlay) = &overlay {
                    self.routing.dial_connected(overlay);
                }
            } else {
                trace!(peer_id = %established.peer_id, "ConnectionEstablished for untracked outbound peer");
            }
        } else {
            self.connection_registry.connected_inbound(
                established.peer_id,
                established.connection_id,
            );
            gauge!("peer_registry_pending_connections").increment(1.0);
        }
    }

    fn handle_connection_closed(&mut self, closed: libp2p::swarm::behaviour::ConnectionClosed) {
        if closed.remaining_established > 0 {
            return;
        }

        // Remove from connection registry (sole source of truth for connections)
        let removed_state = self.connection_registry.disconnected(&closed.peer_id);
        if let Some(ref s) = removed_state {
            decrement_connection_phase_gauge(s);
        }
        let connected_at = removed_state.as_ref().and_then(|s| s.connected_at());
        let overlay = removed_state.as_ref().and_then(|s| s.id());

        self.gossip.send(GossipInput::ConnectionClosed {
            peer_id: closed.peer_id,
            overlay: overlay.clone(),
        });

        let Some(overlay) = overlay else {
            // Unknown overlay connection closed — no routing capacity to release and
            // no routing table entry to update, so skip evaluation.
            self.metrics.record_unknown_overlay_disconnect();
            return;
        };

        // Use the node_type recorded at PeerReady time for symmetric metric decrement.
        let node_type = self.connected_node_types.remove(&overlay)
            .unwrap_or(SwarmNodeType::Client);

        let connection_duration = connected_at.map(|t| t.elapsed());
        debug!(
            peer_id = %closed.peer_id,
            %overlay,
            ?node_type,
            ?connection_duration,
            cause = ?closed.cause,
            "Peer disconnected"
        );

        // Release capacity slot
        RoutingCapacity::disconnected(&*self.routing, &overlay);

        // Push event-driven routing gauges for the affected bin
        let po = self.proximity(&overlay);
        self.push_routing_gauges(po);

        // Capacity freed - coalesced evaluation in poll()
        self.evaluator_handle.trigger_evaluation();

        // Update routing tables
        let old_depth = self.routing.depth();
        SwarmRouting::on_peer_disconnected(&*self.routing, &overlay);
        let new_depth = self.routing.depth();

        // Determine disconnect reason from pending evictions and libp2p cause.
        let disconnect_reason = if self.pending_evictions.remove(&overlay) {
            DisconnectReason::BinTrimmed
        } else {
            match closed.cause {
                Some(ConnectionError::IO(_)) => DisconnectReason::ConnectionError,
                Some(ConnectionError::KeepAliveTimeout) => DisconnectReason::ConnectionError,
                // No error: orderly close initiated by local or remote side.
                None => DisconnectReason::LocalClose,
            }
        };

        // Penalize early disconnects (post-handshake connections that fail quickly).
        // Skip BinTrimmed since we initiated the eviction.
        if disconnect_reason != DisconnectReason::BinTrimmed {
            if let Some(duration) = connection_duration {
                if duration < self.early_disconnect_threshold {
                    debug!(
                        %overlay,
                        ?duration,
                        ?disconnect_reason,
                        "early disconnect detected, applying penalty"
                    );
                    self.peer_manager.record_early_disconnect(&overlay, duration);
                    self.metrics.record_early_disconnect(disconnect_reason);
                }
            }
        }

        self.emit_event(TopologyEvent::PeerDisconnected {
            overlay,
            reason: disconnect_reason,
            connection_duration,
            node_type,
        });

        if new_depth != old_depth {
            self.push_bin_targets();
            self.gossip.send(GossipInput::DepthChanged(new_depth));
            self.emit_event(TopologyEvent::DepthChanged {
                old_depth,
                new_depth,
            });
            if new_depth > old_depth {
                self.trim_overpopulated_bins();
            }
        }
    }

    fn handle_dial_failure(&mut self, failure: libp2p::swarm::behaviour::DialFailure) {
        let Some(peer_id) = failure.peer_id else {
            trace!("DialFailure without peer_id");
            return;
        };

        // Resolve from DialTracker (sole source of outbound dial tracking)
        let Some(request) = self.dial_tracker.resolve(&peer_id) else {
            trace!(%peer_id, "DialFailure for unknown/untracked peer_id");
            return;
        };

        let overlay = request.id;
        let dial_duration = Some(request.queued_at().elapsed());

        let classified_error = classify_dial_error(&failure.error);

        // Release routing capacity for this failed dial
        if let Some(overlay) = &overlay {
            self.routing.release_dial(overlay);
            self.peer_manager.record_dial_failure(overlay);

            // Score penalty based on error type
            use vertex_swarm_peer_score::SwarmScoringEvent;
            let scoring_event = match &classified_error {
                DialError::Timeout | DialError::Stale => SwarmScoringEvent::ConnectionTimeout,
                DialError::ConnectionRefused => SwarmScoringEvent::ConnectionRefused,
                _ => SwarmScoringEvent::ConnectionTimeout,
            };
            self.peer_manager.record_scoring_event(overlay, scoring_event);
        }

        warn!(
            %peer_id,
            ?overlay,
            ?classified_error,
            addr_count = request.addrs.len(),
            "Dial failed (all addresses exhausted)"
        );

        self.emit_event(TopologyEvent::DialFailed {
            overlay,
            addrs: request.addrs,
            error: classified_error,
            dial_duration,
            reason: Some(request.data),
        });
    }

    /// Clean up pending connections that have been waiting longer than HANDSHAKE_TIMEOUT.
    ///
    /// This includes both:
    /// - Dials stuck waiting for TCP/QUIC connection (can take 2+ minutes due to OS retries)
    /// - Handshakes stuck waiting for peer to complete the handshake protocol
    ///
    /// This cleanup ensures stuck connections don't block new connection attempts.
    fn cleanup_stale_pending(&mut self) {
        // Clean up stale dials from the DialTracker (covers all outbound dials)
        let cleanup = self.dial_tracker.cleanup_expired();
        for request in cleanup.timed_out_in_flight {
            if let Some(overlay) = &request.id {
                self.routing.release_dial(overlay);
                self.peer_manager.record_dial_failure(overlay);
            }
            warn!(
                peer_id = %request.peer_id,
                overlay = ?request.id,
                timeout = ?HANDSHAKE_TIMEOUT,
                "Cleaning up stale dial from tracker"
            );
            let dial_duration = request.queued_at().elapsed();
            self.emit_event(TopologyEvent::DialFailed {
                overlay: request.id,
                addrs: request.addrs,
                error: DialError::Stale,
                dial_duration: Some(dial_duration),
                reason: Some(request.data),
            });
        }

        // Clean up stale handshakes from the connection registry
        // (connections that established TCP but handshake hasn't completed)
        let stale_peers = self.connection_registry.stale_pending(HANDSHAKE_TIMEOUT);

        for peer_id in stale_peers {
            if let Some(state) = self.connection_registry.disconnected(&peer_id) {
                decrement_connection_phase_gauge(&state);

                let reason = *state.reason();
                let overlay = state.id();

                if let Some(overlay) = &overlay {
                    self.routing.release_handshake(overlay);
                    self.peer_manager.record_dial_failure(overlay);
                }

                warn!(
                    %peer_id,
                    ?overlay,
                    timeout = ?HANDSHAKE_TIMEOUT,
                    "Cleaning up stale handshake"
                );

                self.emit_event(TopologyEvent::DialFailed {
                    overlay,
                    addrs: Vec::new(),
                    error: DialError::Stale,
                    dial_duration: state.started_at().map(|t| t.elapsed()),
                    reason,
                });
            }
        }
    }

    // Peer store

    /// Collect dirty hot peers and flush all pending writes to persistent storage.
    fn save_peers(&self) {
        if self.peer_store.is_some() {
            self.peer_manager.collect_dirty();
            self.peer_manager.flush_write_buffer();
            debug!(peers = self.peer_manager.index().len(), "Flushed peers to store");
        }
    }

    fn broadcast_peers(&mut self, to: OverlayAddress, peers: Vec<SwarmPeer>) {
        let Some(state) = self.connection_registry.get(&to) else {
            warn!(%to, "Cannot broadcast: peer not found");
            return;
        };
        if let Some(connection_id) = state.connection_id() {
            let peer_id = state.peer_id();
            for chunk in peers.chunks(MAX_BATCH_SIZE) {
                self.protocols.hive.broadcast(peer_id, connection_id, chunk.to_vec());
            }
        }
    }

    // Routing

    /// Drain candidates from the background evaluator's per-bin queues and dial them.
    fn drain_candidate_queues(&mut self) {
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
    fn trim_overpopulated_bins(&mut self) {
        let candidates = self.routing.eviction_candidates();
        if candidates.is_empty() {
            return;
        }

        let mut trimmed = 0;
        for candidate in &candidates {
            let reason = self.connection_registry.get(&candidate.overlay)
                .and_then(|s| *s.reason());
            if matches!(reason, Some(DialReason::Trusted)) {
                continue;
            }

            let Some(peer_id) = self.connection_registry.resolve_peer_id(&candidate.overlay)
            else {
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
            info!(trimmed, total_candidates = candidates.len(), "Trimmed overpopulated bins");
        }
    }

    // Metrics and helpers

    fn emit_event(&self, event: TopologyEvent) {
        self.metrics.record_event(&event);
        let _ = self.event_tx.send(event);
    }

    /// Push routing gauges for a single bin and global totals.
    fn push_routing_gauges(&self, po: u8) {
        let po_str = po_label(po);
        let (connected, known) = self.routing.bin_peer_counts(po);
        let (dialing, handshaking, active) = self.routing.bin_phase_counts(po);

        metrics::gauge!("topology_bin_connected_peers", "po" => po_str)
            .set(connected as f64);
        metrics::gauge!("topology_bin_known_peers", "po" => po_str)
            .set(known as f64);
        metrics::gauge!("topology_bin_dialing", "po" => po_str)
            .set(dialing as f64);
        metrics::gauge!("topology_bin_handshaking", "po" => po_str)
            .set(handshaking as f64);
        metrics::gauge!("topology_bin_active", "po" => po_str)
            .set(active as f64);
        metrics::gauge!("topology_bin_effective", "po" => po_str)
            .set((dialing + handshaking + active) as f64);
    }

    /// Push per-bin target/ceiling gauges and the global nominal gauge (called on depth change).
    fn push_bin_targets(&self) {
        let depth = self.routing.depth();
        let limits = self.routing.limits();
        let bin_count = self.routing.bin_sizes().len();

        for po in 0..bin_count {
            let po_str = po_label(po as u8);
            let target = limits.target(po as u8, depth);
            let target_val = if target == usize::MAX { -1.0 } else { target as f64 };
            let ceiling_val = limits.ceiling(po as u8, depth);
            let ceiling = if ceiling_val == usize::MAX { -1.0 } else { ceiling_val as f64 };

            metrics::gauge!("topology_bin_target_peers", "po" => po_str).set(target_val);
            metrics::gauge!("topology_bin_ceiling_peers", "po" => po_str).set(ceiling);
        }

        metrics::gauge!("topology_bin_nominal_peers").set(limits.nominal() as f64);
    }

    /// Get the proximity order for a peer relative to our overlay address.
    fn proximity(&self, peer: &OverlayAddress) -> u8 {
        self.identity.overlay_address().proximity(peer)
    }

    /// Check if we can advertise to a peer based on address scope.
    ///
    /// - Public peers: require public addresses (NAT or discovered)
    /// - Private peers on LAN: can use private addresses if on same subnet
    /// - Loopback peers: always dialable
    fn can_advertise_to(&self, peer: &SwarmPeer) -> bool {
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
                    listen_addrs.iter().any(|our_addr| same_subnet(our_addr, peer_addr))
                })
            }
            Some(AddressScope::Loopback) | None => {
                // Loopback or unknown - allow
                true
            }
        }
    }
}

/// Decrement the appropriate connection phase gauge based on the removed state.
fn decrement_connection_phase_gauge<Id: Clone, R>(state: &ConnectionState<Id, R>) {
    if state.is_active() {
        gauge!("peer_registry_active_connections").decrement(1.0);
    } else if state.is_pending() {
        gauge!("peer_registry_pending_connections").decrement(1.0);
    }
}

/// Classify a libp2p dial error into a structured `DialError` variant.
fn classify_dial_error(error: &libp2p::swarm::DialError) -> DialError {
    use std::io::ErrorKind;
    use libp2p::core::transport::TransportError;

    match error {
        libp2p::swarm::DialError::Transport(addrs) => {
            // Classify based on the most informative transport error.
            // If all addresses failed with the same kind, use that; otherwise fall back.
            for (_, err) in addrs {
                match err {
                    TransportError::Other(io_err) => match io_err.kind() {
                        ErrorKind::TimedOut => return DialError::Timeout,
                        ErrorKind::ConnectionRefused => return DialError::ConnectionRefused,
                        ErrorKind::AddrNotAvailable | ErrorKind::NetworkUnreachable | ErrorKind::HostUnreachable
                            => return DialError::Unreachable,
                        _ => {
                            // Check inner error message for nested timeout/refused
                            let msg = io_err.to_string().to_lowercase();
                            if msg.contains("timed out") || msg.contains("timeout") {
                                return DialError::Timeout;
                            }
                            if msg.contains("connection refused") {
                                return DialError::ConnectionRefused;
                            }
                            if msg.contains("no route") {
                                return DialError::NoRoute;
                            }
                            if msg.contains("unreachable") {
                                return DialError::Unreachable;
                            }
                            if msg.contains("negotiation") || msg.contains("multistream") {
                                return DialError::NegotiationFailed;
                            }
                        }
                    },
                    TransportError::MultiaddrNotSupported(_) => {}
                }
            }
            DialError::Other(format!("{error:?}"))
        }
        libp2p::swarm::DialError::Aborted
        | libp2p::swarm::DialError::DialPeerConditionFalse(_) => DialError::Stale,
        libp2p::swarm::DialError::Denied { .. } => DialError::NegotiationFailed,
        libp2p::swarm::DialError::NoAddresses => DialError::NoRoute,
        libp2p::swarm::DialError::LocalPeerId { .. }
        | libp2p::swarm::DialError::WrongPeerId { .. } => {
            DialError::Other(format!("{error:?}"))
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
        self.protocols.on_swarm_event(event.clone());

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
                let capability_became_known = self.nat_discovery.on_new_listen_addr(info.addr.clone());

                if capability_became_known {
                    debug!("Network capability now known, triggering immediate dial");
                    self.evaluator_handle.trigger_evaluation();
                }
            }
            FromSwarm::ExpiredListenAddr(info) => {
                debug!(address = %info.addr, "Expired listen address");
                self.nat_discovery.on_expired_listen_addr(&info.addr);
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
        self.protocols.on_connection_handler_event(peer_id, connection_id, event);
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        // Use TimingGuard for automatic poll duration recording
        let _poll_timer = vertex_observability::TimingGuard::new(
            metrics::histogram!("topology_poll_duration_seconds"),
        );

        // Emit any pending static NAT addresses as external addresses (one-time on startup)
        if !self.pending_nat_external_addrs.is_empty() {
            for addr in std::mem::take(&mut self.pending_nat_external_addrs) {
                debug!(addr = %addr, "Emitting static NAT address as external address");
                self.pending_actions.push_back(ToSwarm::ExternalAddrConfirmed(addr));
            }
        }

        // Poll for commands from TopologyHandle
        while let Poll::Ready(Some(command)) = self.command_rx.poll_recv(cx) {
            self.on_command(command);
        }

        // Poll pending dnsaddr resolution for bootnodes
        if let Some(ref mut future) = self.pending_bootnode_resolution {
            if let Poll::Ready((resolved_bootnodes, resolved_trusted)) = future.as_mut().poll(cx) {
                info!(
                    bootnodes = resolved_bootnodes.len(),
                    trusted = resolved_trusted.len(),
                    "dnsaddr resolution complete, dialing bootnodes"
                );
                self.pending_bootnode_resolution = None;
                self.dial_bootnodes(resolved_bootnodes, resolved_trusted);
            }
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
                Poll::Ready(ToSwarm::NotifyHandler { peer_id, handler, event }) => {
                    return Poll::Ready(ToSwarm::NotifyHandler { peer_id, handler, event });
                }
                Poll::Ready(ToSwarm::CloseConnection { peer_id, connection }) => {
                    return Poll::Ready(ToSwarm::CloseConnection { peer_id, connection });
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
