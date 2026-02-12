//! Network topology behaviour managing peer connections via handshake, hive, and pingpong.

use std::{
    collections::VecDeque,
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
    multiaddr::Protocol,
    swarm::{
        ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, NotifyHandler,
        THandlerInEvent, ToSwarm, dial_opts::DialOpts,
    },
};
use tracing::{debug, info, trace, warn};
use vertex_net_hive::MAX_BATCH_SIZE;
use vertex_net_local::LocalCapabilities;
use vertex_swarm_api::{SwarmBootnodeConfig, SwarmIdentity};
use vertex_swarm_peer::SwarmPeer;
use vertex_swarm_peer_manager::{InternalPeerManager, PeerManager};
use vertex_swarm_primitives::OverlayAddress;

use vertex_swarm_peer_registry::{ActivateResult, SwarmPeerRegistry as ConnectionRegistry};
use crate::DialReason;
use crate::dns::{is_dnsaddr, resolve_all_dnsaddrs};
use crate::error::TopologyError;
use vertex_net_handshake::HANDSHAKE_TIMEOUT;
use crate::events::{ConnectionDirection, DisconnectReason, RejectionReason, TopologyEvent};
use crate::gossip::{Gossip, GossipAction, GossipCommand};
use crate::handle::TopologyHandle;
use crate::handler::{Command, Event, HandlerConfig, TopologyHandler};
use crate::metrics::TopologyMetrics;
use crate::nat_discovery::{NatDiscovery, NatDiscoveryConfig};
use crate::routing::{KademliaConfig, KademliaRouting, RoutingCapacity, SwarmRouting};
use crate::TopologyCommand;

pub const DEFAULT_DIAL_INTERVAL: Duration = Duration::from_secs(5);

const EVENT_CHANNEL_CAPACITY: usize = 256;
const COMMAND_CHANNEL_CAPACITY: usize = 64;
const MIN_BOOTNODE_CONNECTIONS: usize = 1;

/// Extract PeerId from a multiaddr's /p2p/ component.
fn extract_peer_id(addr: &Multiaddr) -> Option<PeerId> {
    addr.iter().find_map(|p| match p {
        Protocol::P2p(id) => Some(id),
        _ => None,
    })
}

/// Target for dialing a peer.
#[derive(Debug, Clone)]
pub enum DialTarget {
    /// Known peer from gossip/store - has overlay for verification during handshake.
    Known(SwarmPeer),

    /// Unknown peer - overlay learned at handshake.
    /// Multiaddr must contain /p2p/ component.
    Unknown(Multiaddr),
}

impl DialTarget {
    /// Get the PeerId for this dial target, if present in the multiaddr.
    pub fn peer_id(&self) -> Option<PeerId> {
        match self {
            Self::Known(peer) => peer.multiaddrs().iter().find_map(extract_peer_id),
            Self::Unknown(addr) => extract_peer_id(addr),
        }
    }

    /// Get the overlay address if known.
    pub fn overlay(&self) -> Option<OverlayAddress> {
        match self {
            Self::Known(peer) => Some(OverlayAddress::from(*peer.overlay())),
            Self::Unknown(_) => None,
        }
    }

    /// Get the addresses to dial.
    pub fn addrs(&self) -> Vec<Multiaddr> {
        match self {
            Self::Known(peer) => peer.multiaddrs().to_vec(),
            Self::Unknown(addr) => vec![addr.clone()],
        }
    }
}

/// Configuration for topology behaviour and handler.
#[derive(Debug, Clone)]
pub struct TopologyConfig {
    // Behaviour settings
    pub kademlia: KademliaConfig,
    pub dial_interval: Duration,
    pub nat: NatDiscoveryConfig,
    pub nat_auto: bool,

    // Handler/protocol settings
    pub hive_timeout: Duration,
    pub pingpong_timeout: Duration,
    pub pingpong_greeting: String,
}

impl Default for TopologyConfig {
    fn default() -> Self {
        Self {
            kademlia: KademliaConfig::default(),
            dial_interval: DEFAULT_DIAL_INTERVAL,
            nat: NatDiscoveryConfig::default(),
            nat_auto: false,
            hive_timeout: Duration::from_secs(60),
            pingpong_timeout: Duration::from_secs(30),
            pingpong_greeting: "ping".to_string(),
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

    pub fn with_nat_auto(mut self, enabled: bool) -> Self {
        self.nat_auto = enabled;
        self
    }

    fn handler_config(&self) -> HandlerConfig {
        HandlerConfig {
            hive_timeout: self.hive_timeout,
            pingpong_timeout: self.pingpong_timeout,
            pingpong_greeting: self.pingpong_greeting.clone(),
        }
    }
}

/// Network topology behaviour managing peer connections.
///
/// Creates and owns all internal state (routing, peer_manager, dial_tracker, etc.)
/// and provides a [`TopologyHandle`] for external queries and commands.
pub struct TopologyBehaviour<I: SwarmIdentity> {
    config: TopologyConfig,
    identity: Arc<I>,

    // Shared with TopologyHandle (Arc for external access)
    routing: Arc<KademliaRouting<I>>,
    peer_manager: Arc<PeerManager>,

    // Owned (internal only, Arc for handler sharing and routing integration)
    connection_registry: Arc<ConnectionRegistry>,
    nat_discovery: Arc<NatDiscovery>,
    bootnodes: Vec<Multiaddr>,
    trusted_peers: Vec<Multiaddr>,

    // Channels
    command_rx: mpsc::Receiver<TopologyCommand>,
    event_tx: broadcast::Sender<TopologyEvent>,

    // Connection state
    pending_actions: VecDeque<ToSwarm<(), Command>>,

    // Gossip coordination
    gossip: Gossip,

    // Periodic dial interval
    dial_interval: Pin<Box<Interval>>,

    // Pending dnsaddr resolution for bootnodes
    pending_bootnode_resolution: Option<Pin<Box<dyn Future<Output = Vec<Multiaddr>> + Send>>>,

    // Metrics
    metrics: TopologyMetrics,
}

impl<I: SwarmIdentity> TopologyBehaviour<I> {
    /// Set the local PeerId for address advertisement in handshakes.
    ///
    /// Must be called after the libp2p Swarm is built. All multiaddrs
    /// advertised to peers will include `/p2p/{peer_id}`.
    pub fn set_local_peer_id(&self, peer_id: PeerId) {
        self.nat_discovery.set_local_peer_id(peer_id);
    }

    fn emit_event(&self, event: TopologyEvent) {
        self.metrics.record_event(&event);
        let _ = self.event_tx.send(event);
    }

    /// Get the proximity order for a peer relative to our overlay address.
    fn proximity(&self, peer: &OverlayAddress) -> u8 {
        self.identity.overlay_address().proximity(peer)
    }
}

impl<I: SwarmIdentity + Clone> TopologyBehaviour<I> {
    /// Create topology behaviour and handle.
    pub fn new(
        identity: I,
        config: TopologyConfig,
        network_config: &impl SwarmBootnodeConfig,
    ) -> Result<(Self, TopologyHandle<I>), TopologyError> {
        let bootnodes = network_config.bootnodes().to_vec();
        let trusted_peers = network_config.trusted_peers().to_vec();
        let nat_addrs = network_config.nat_addrs().to_vec();
        let nat_auto = network_config.nat_auto_enabled() || config.nat_auto;

        let (event_tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let (command_tx, command_rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);

        let peer_manager = Arc::new(PeerManager::from_config(network_config.peers())?);

        let connection_registry = Arc::new(ConnectionRegistry::new());

        let routing = KademliaRouting::new(identity.clone(), config.kademlia.clone());

        let known_peers = peer_manager.known_peers();
        if !known_peers.is_empty() {
            // Filter out our own overlay to prevent self-dial
            let local_overlay = identity.overlay_address();
            let filtered_peers: Vec<_> = known_peers
                .into_iter()
                .filter(|peer| {
                    if *peer == local_overlay {
                        warn!("Filtered self-overlay from stored peers (corrupted peer store?)");
                        false
                    } else {
                        true
                    }
                })
                .collect();

            if !filtered_peers.is_empty() {
                info!(count = filtered_peers.len(), "seeding kademlia with stored peers");
                routing.add_peers(&filtered_peers);
            }
        }

        let local_capabilities = Arc::new(LocalCapabilities::new());

        let nat_discovery = Arc::new(if nat_auto || !nat_addrs.is_empty() {
            if nat_auto {
                info!("Auto NAT discovery enabled");
            }
            if !nat_addrs.is_empty() {
                info!(count = nat_addrs.len(), "NAT addresses configured");
            }
            NatDiscovery::new(
                local_capabilities.clone(),
                nat_addrs,
                config.nat.clone(),
                nat_auto,
            )
        } else {
            NatDiscovery::disabled(local_capabilities.clone())
        });

        let mut gossip =
            Gossip::new(identity.overlay_address(), peer_manager.clone(), connection_registry.clone());
        gossip.set_depth(routing.depth());

        let identity = Arc::new(identity);

        let handle = TopologyHandle::new(
            identity.clone(),
            routing.clone(),
            connection_registry.clone(),
            peer_manager.clone(),
            command_tx,
            event_tx.clone(),
        );

        let dial_interval = config.dial_interval;

        let behaviour = Self {
            config,
            identity,
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
            pending_bootnode_resolution: None,
            metrics: TopologyMetrics::new(),
        };

        Ok((behaviour, handle))
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

            // Spawn async resolution - results will be processed in poll()
            let future = Box::pin(async move {
                let mut all_addrs = Vec::new();
                all_addrs.extend(resolve_all_dnsaddrs(bootnodes.iter()).await);
                all_addrs.extend(resolve_all_dnsaddrs(trusted_peers.iter()).await);
                all_addrs
            });
            self.pending_bootnode_resolution = Some(future);
        } else {
            // No resolution needed, dial immediately
            self.dial_bootnodes(bootnodes, trusted_peers);
        }
    }

    /// Dial bootnodes and trusted peers (called after dnsaddr resolution if needed).
    fn dial_bootnodes(&mut self, bootnodes: Vec<Multiaddr>, trusted_peers: Vec<Multiaddr>) {
        let bootnode_count = bootnodes.len();
        if bootnode_count > 0 {
            info!(count = bootnode_count, "Connecting to bootnodes...");
        }

        let mut dialed = 0;
        for addr in bootnodes {
            if dialed >= MIN_BOOTNODE_CONNECTIONS {
                info!(dialed, "Reached minimum bootnode connections");
                break;
            }
            self.dial(DialTarget::Unknown(addr), DialReason::Bootnode);
            dialed += 1;
        }

        for addr in trusted_peers {
            self.dial(DialTarget::Unknown(addr), DialReason::Trusted);
        }
    }

    /// Dial a peer target.
    ///
    /// For Known peers: checks routing capacity, registers with overlay, verifies during handshake.
    /// For Unknown peers: no capacity check, registers without overlay, learns it at handshake.
    fn dial(&mut self, target: DialTarget, reason: DialReason) {
        use vertex_net_local::prepare_dial_addresses;

        let Some(peer_id) = target.peer_id() else {
            warn!(?target, "Cannot dial: no /p2p/ component in address");
            return;
        };

        if self.connection_registry.contains_peer(&peer_id) {
            trace!(%peer_id, "Skipping dial - already connected");
            return;
        }

        // For Known peers, check routing capacity before dialing
        if let Some(overlay) = target.overlay() {
            if !self.routing.try_reserve_dial(&overlay, true) {
                trace!(%overlay, "Skipping dial - at capacity or already tracking");
                return;
            }
        }

        let capability = self.nat_discovery.capability().ip;
        let addrs = target.addrs();
        let dial_prep = prepare_dial_addresses(addrs, capability);

        if dial_prep.is_empty() {
            // Release reservation if we can't dial
            if let Some(overlay) = target.overlay() {
                self.routing.release_dial(&overlay);
            }
            debug!(%peer_id, ?capability, "No reachable addresses");
            return;
        }

        let concurrency = dial_prep.concurrency_factor();
        let dial_addrs = dial_prep.into_addrs();

        // Register with connection registry
        let dial_addrs = match target.overlay() {
            Some(overlay) => {
                // Known peer - register with overlay
                self.connection_registry.start_dial(peer_id, overlay, dial_addrs, reason)
            }
            None => {
                // Unknown peer - register without overlay
                self.connection_registry.start_dial_unknown_overlay(peer_id, dial_addrs, reason)
            }
        };

        let Some(dial_addrs) = dial_addrs else {
            // Release reservation if registry rejected
            if let Some(overlay) = target.overlay() {
                self.routing.release_dial(&overlay);
            }
            trace!(%peer_id, "Skipping dial - already tracking");
            return;
        };

        debug!(%peer_id, addr_count = dial_addrs.len(), ?reason, "Dialing peer");

        let opts = DialOpts::peer_id(peer_id)
            .addresses(dial_addrs)
            .override_dial_concurrency_factor(concurrency)
            .build();
        self.pending_actions.push_back(ToSwarm::Dial { opts });
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
                debug!(%overlay, "Banned peer via command");
            }
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
                self.pending_actions.push_back(ToSwarm::NotifyHandler {
                    peer_id,
                    handler: NotifyHandler::One(connection_id),
                    event: Command::BroadcastPeers(chunk.to_vec()),
                });
            }
        }
    }

    fn execute_gossip_actions(&mut self, actions: Vec<GossipAction>) {
        for action in actions {
            self.broadcast_peers(action.to, action.peers);
        }
    }

    fn dial_candidates(&mut self) {
        let candidates = self.routing.peers_to_connect();
        if candidates.is_empty() {
            return;
        }

        let mut dialable = self.peer_manager.get_dialable_peers(&candidates);
        let filtered_count = candidates.len() - dialable.len();
        if filtered_count > 0 {
            trace!(
                total_candidates = candidates.len(),
                dialable = dialable.len(),
                filtered = filtered_count,
                "Candidates filtered by peer state"
            );
        }

        // Shuffle candidates to add randomness and prevent all nodes from
        // dialing the same peers in the same order (thundering herd mitigation).
        dialable.shuffle(&mut rand::rng());

        for swarm_peer in dialable {
            self.dial(DialTarget::Known(swarm_peer), DialReason::Discovery);
        }
    }

    /// Clean up pending connections that have been waiting longer than HANDSHAKE_TIMEOUT.
    ///
    /// This includes both:
    /// - Dials stuck waiting for TCP/QUIC connection (can take 2+ minutes due to OS retries)
    /// - Handshakes stuck waiting for peer to complete the handshake protocol
    ///
    /// This cleanup ensures stuck connections don't block new connection attempts.
    fn cleanup_stale_pending(&mut self) {
        let stale_peers = self.connection_registry.stale_pending(HANDSHAKE_TIMEOUT);

        for peer_id in stale_peers {
            if let Some(state) = self.connection_registry.complete_dial(&peer_id) {
                let overlay = state.id();
                let is_handshake = state.is_handshaking();

                // Release routing capacity
                if let Some(overlay) = &overlay {
                    self.routing.release_dial(overlay);
                    // Record failure for exponential backoff
                    self.peer_manager.record_dial_failure(overlay);
                }

                let error_msg = if is_handshake {
                    "handshake timeout"
                } else {
                    "dial timeout"
                };

                warn!(
                    %peer_id,
                    ?overlay,
                    timeout = ?HANDSHAKE_TIMEOUT,
                    state = if is_handshake { "handshaking" } else { "dialing" },
                    "Cleaning up stale connection attempt"
                );

                self.emit_event(TopologyEvent::DialFailed {
                    overlay,
                    addrs: state.addrs().cloned().unwrap_or_default(),
                    error: error_msg.to_string(),
                    dial_duration: state.started_at().map(|t| t.elapsed()),
                    reason: overlay.and_then(|o| self.connection_registry.dial_reason(&o)),
                });
            }
        }
    }

    fn process_handler_event(
        &mut self,
        peer_id: PeerId,
        connection_id: ConnectionId,
        event: Event,
    ) {
        match event {
            Event::HandshakeCompleted { info, handshake_duration } => {
                let overlay = OverlayAddress::from(*info.swarm_peer.overlay());
                let storer = info.node_type.requires_storage();
                debug!(
                    %peer_id,
                    %overlay,
                    %storer,
                    po = self.proximity(&overlay),
                    ?handshake_duration,
                    "Handshake completed"
                );

                // Get dial info from connection registry before transitioning
                let current_state = self.connection_registry.get(&overlay)
                    .or_else(|| self.connection_registry.resolve_overlay(&peer_id)
                        .and_then(|o| self.connection_registry.get(&o)));
                let direction = current_state.as_ref()
                    .and_then(|s| s.direction())
                    .unwrap_or(ConnectionDirection::Inbound);

                // Transition to Active state in connection registry
                let activate_result = self.connection_registry.handshake_completed(
                    peer_id,
                    connection_id,
                    overlay,
                );

                // Update routing capacity tracking
                RoutingCapacity::handshake_completed(&*self.routing, &overlay);

                let bin_at_capacity = !self.routing.should_accept_peer(&overlay, storer);
                if bin_at_capacity {
                    debug!(
                        %peer_id,
                        %overlay,
                        %storer,
                        ?direction,
                        "Rejecting connection: bin saturated"
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

                // Handle the activate result from connection registry
                match activate_result {
                    ActivateResult::Replaced { old_peer_id, old_connection_id, .. } => {
                        debug!(
                            %peer_id,
                            %old_peer_id,
                            ?old_connection_id,
                            %overlay,
                            "Closing old connection, new connection takes over"
                        );
                        self.pending_actions.push_back(ToSwarm::CloseConnection {
                            peer_id: old_peer_id,
                            connection: libp2p::swarm::CloseConnection::All,
                        });
                    }
                    ActivateResult::Accepted => {}
                }

                // Store peer metadata
                self.peer_manager.on_peer_ready(info.swarm_peer.clone(), storer);

                let old_depth = self.routing.depth();
                self.routing.connected(overlay);
                let new_depth = self.routing.depth();

                if new_depth != old_depth {
                    self.gossip.set_depth(new_depth);
                    self.emit_event(TopologyEvent::DepthChanged {
                        old_depth,
                        new_depth,
                    });
                }

                self.emit_event(TopologyEvent::PeerReady {
                    overlay,
                    peer_id,
                    storer,
                    handshake_duration,
                    direction,
                });

                if let Some(GossipCommand::SendPing(ping_peer_id)) =
                    self.gossip.on_handshake_completed(
                        peer_id,
                        info.swarm_peer,
                        storer,
                    )
                {
                    // Use the connection_id we just established
                    self.pending_actions.push_back(ToSwarm::NotifyHandler {
                        peer_id: ping_peer_id,
                        handler: NotifyHandler::One(connection_id),
                        event: Command::Ping { greeting: None },
                    });
                    debug!(%peer_id, %overlay, "Sent immediate health check ping");
                } else {
                    debug!(%peer_id, %overlay, "Scheduled delayed health check ping (gossip dial)");
                }

                // Dial completed successfully - evaluate for more candidates
                self.routing.evaluate_connections();
                self.dial_candidates();
            }
            Event::HandshakeFailed { error, handshake_duration } => {
                warn!(%peer_id, %error, ?handshake_duration, "Handshake failed");

                // Get dial info before removing from registry
                let state = self.connection_registry.complete_dial(&peer_id);

                // Release routing capacity for this failed handshake
                let overlay = state.as_ref().and_then(|s| s.id());
                if let Some(ref overlay) = overlay {
                    self.routing.release_handshake(overlay);
                    // Record failure for exponential backoff
                    self.peer_manager.record_dial_failure(overlay);
                }

                self.emit_event(TopologyEvent::DialFailed {
                    overlay: overlay.clone(),
                    addrs: state.as_ref().and_then(|s| s.addrs().cloned()).unwrap_or_default(),
                    error: error.to_string(),
                    dial_duration: state.as_ref().and_then(|s| s.started_at()).map(|t| t.elapsed()),
                    reason: overlay.and_then(|o| self.connection_registry.dial_reason(&o)),
                });
            }
            Event::HivePeersReceived(peers) => {
                if !peers.is_empty() {
                    let from = self
                        .connection_registry
                        .resolve_overlay(&peer_id)
                        .unwrap_or_else(|| {
                            warn!(%peer_id, "Hive peers from unknown peer");
                            OverlayAddress::default()
                        });
                    debug!(%peer_id, %from, count = peers.len(), "Peers received via hive");

                    let stored_overlays = self.peer_manager.store_discovered_peers(peers);

                    if !stored_overlays.is_empty() {
                        self.routing.add_peers(&stored_overlays);
                        self.routing.evaluate_connections();
                        self.dial_candidates();
                    }
                }

                // Check if we should disconnect after peer exchange (bin at capacity)
                if let Some(overlay) = self.connection_registry.resolve_overlay(&peer_id) {
                    if self.connection_registry.is_gossip_disconnect_pending(&overlay) {
                        debug!(
                            %peer_id,
                            %overlay,
                            "Disconnecting gossip peer after peer exchange (bin at capacity)"
                        );
                        self.pending_actions.push_back(ToSwarm::CloseConnection {
                            peer_id,
                            connection: libp2p::swarm::CloseConnection::All,
                        });
                    }
                }
            }
            Event::HiveBroadcastComplete => {
                debug!(%peer_id, "Hive broadcast complete");
            }
            Event::HiveError(error) => {
                warn!(%peer_id, %error, "Hive error");
            }
            Event::PingpongPong { rtt } => {
                debug!(%peer_id, ?rtt, "Pingpong success");

                if let Some(overlay) = self.connection_registry.resolve_overlay(&peer_id) {
                    self.peer_manager.record_latency(&overlay, rtt);
                    debug!(%peer_id, %overlay, ?rtt, "Connection health verified, triggering gossip");

                    self.emit_event(TopologyEvent::PingCompleted {
                        overlay,
                        rtt,
                    });
                }

                let gossip_actions = self.gossip.on_pong_received(peer_id);
                self.execute_gossip_actions(gossip_actions);
            }
            Event::PingpongPingReceived => {
                debug!(%peer_id, "Received ping from peer");
            }
            Event::PingpongError(error) => {
                warn!(%peer_id, %error, "Pingpong failed");

                if self.gossip.on_ping_error(&peer_id) {
                    debug!(%peer_id, "Cleaned up pending gossip after ping failure");
                }
            }
        }
    }
}

impl<I: SwarmIdentity + Clone> NetworkBehaviour for TopologyBehaviour<I> {
    type ConnectionHandler = TopologyHandler<I>;
    type ToSwarm = ();

    fn handle_established_inbound_connection(
        &mut self,
        _connection_id: ConnectionId,
        peer: PeerId,
        _local_addr: &Multiaddr,
        remote_addr: &Multiaddr,
    ) -> Result<Self::ConnectionHandler, ConnectionDenied> {
        Ok(TopologyHandler::new(
            self.config.handler_config(),
            self.identity.clone(),
            peer,
            remote_addr,
            self.nat_discovery.clone(),
        ))
    }

    fn handle_established_outbound_connection(
        &mut self,
        _connection_id: ConnectionId,
        peer: PeerId,
        addr: &Multiaddr,
        _role_override: Endpoint,
        _port_use: PortUse,
    ) -> Result<Self::ConnectionHandler, ConnectionDenied> {
        Ok(TopologyHandler::new(
            self.config.handler_config(),
            self.identity.clone(),
            peer,
            addr,
            self.nat_discovery.clone(),
        ))
    }

    fn on_swarm_event(&mut self, event: FromSwarm) {
        match event {
            FromSwarm::ConnectionEstablished(established) => {
                if established.endpoint.is_dialer() {
                    let resolved_addr = established.endpoint.get_remote_address().clone();

                    // Transition from Dialing to Handshaking in the registry
                    let state = self.connection_registry.connection_established(
                        established.peer_id,
                        established.connection_id,
                    );

                    // Transition routing capacity from Dialing to Handshaking
                    if let Some(overlay) = state.as_ref().and_then(|s| s.id()) {
                        self.routing.dial_connected(&overlay);
                    }

                    self.pending_actions.push_back(ToSwarm::NotifyHandler {
                        peer_id: established.peer_id,
                        handler: NotifyHandler::One(established.connection_id),
                        event: Command::StartHandshake(resolved_addr),
                    });
                } else {
                    // Inbound connection - create Handshaking state
                    self.connection_registry.inbound_connection(
                        established.peer_id,
                        established.connection_id,
                    );
                }
            }
            FromSwarm::ConnectionClosed(closed) => {
                if closed.remaining_established == 0 {
                    // Extract connected_at BEFORE removing from registry
                    let connected_at = self
                        .connection_registry
                        .get_by_peer_id(&closed.peer_id)
                        .and_then(|s| s.connected_at());

                    // Remove from connection registry (sole source of truth for connections)
                    let removed_state = self.connection_registry.disconnected(&closed.peer_id);
                    let overlay = removed_state.and_then(|s| s.id());

                    let gossip_actions = self
                        .gossip
                        .on_connection_closed(&closed.peer_id, overlay.as_ref());
                    self.execute_gossip_actions(gossip_actions);

                    if let Some(overlay) = overlay {
                        let connection_duration = connected_at.map(|t| t.elapsed());
                        debug!(
                            peer_id = %closed.peer_id,
                            %overlay,
                            ?connection_duration,
                            "Peer disconnected"
                        );

                        // Release capacity slot
                        RoutingCapacity::disconnected(&*self.routing, &overlay);

                        // Capacity freed - evaluate for new dial candidates
                        self.routing.evaluate_connections();
                        self.dial_candidates();

                        // Update routing tables
                        let old_depth = self.routing.depth();
                        SwarmRouting::on_peer_disconnected(&*self.routing, &overlay);
                        let new_depth = self.routing.depth();

                        self.emit_event(TopologyEvent::PeerDisconnected {
                            overlay,
                            reason: DisconnectReason::Unknown,
                            connection_duration,
                        });

                        if new_depth != old_depth {
                            self.gossip.set_depth(new_depth);
                            self.emit_event(TopologyEvent::DepthChanged {
                                old_depth,
                                new_depth,
                            });
                        }
                    }
                }
            }
            FromSwarm::DialFailure(failure) => {
                // libp2p now handles address iteration via ConcurrentDial.
                // When we get DialFailure, all addresses have been exhausted.
                if let Some(peer_id) = failure.peer_id {
                    if let Some(state) = self.connection_registry.complete_dial(&peer_id) {
                        let overlay = state.id();
                        let addrs = state.addrs().cloned().unwrap_or_default();
                        let dial_duration = state.started_at().map(|t| t.elapsed());
                        let reason = overlay.and_then(|o| self.connection_registry.dial_reason(&o));

                        // Release routing capacity for this failed dial
                        if let Some(overlay) = &overlay {
                            self.routing.release_dial(overlay);
                            // Record failure for exponential backoff
                            self.peer_manager.record_dial_failure(overlay);
                        }

                        warn!(
                            %peer_id,
                            ?overlay,
                            addr_count = addrs.len(),
                            "Dial failed (all addresses exhausted)"
                        );

                        self.emit_event(TopologyEvent::DialFailed {
                            overlay,
                            addrs,
                            error: format!("{:?}", failure.error),
                            dial_duration,
                            reason,
                        });
                    } else {
                        trace!(
                            %peer_id,
                            "DialFailure for unknown/untracked peer_id"
                        );
                    }
                } else {
                    trace!("DialFailure without peer_id");
                }
            }
            FromSwarm::NewListenAddr(info) => {
                debug!(address = %info.addr, "New listen address");
                let capability_became_known = self.nat_discovery.on_new_listen_addr(info.addr.clone());

                if capability_became_known {
                    debug!("Network capability now known, triggering immediate dial");
                    self.routing.evaluate_connections();
                    self.dial_candidates();
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
        event: Event,
    ) {
        self.process_handler_event(peer_id, connection_id, event);
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        // Poll for commands from TopologyHandle
        while let Poll::Ready(Some(command)) = self.command_rx.poll_recv(cx) {
            self.on_command(command);
        }

        // Poll pending dnsaddr resolution for bootnodes
        if let Some(ref mut future) = self.pending_bootnode_resolution {
            if let Poll::Ready(resolved_addrs) = future.as_mut().poll(cx) {
                info!(
                    count = resolved_addrs.len(),
                    "dnsaddr resolution complete, dialing bootnodes"
                );
                self.pending_bootnode_resolution = None;
                // Split into bootnodes and trusted (we resolved them together, just dial all)
                self.dial_bootnodes(resolved_addrs, Vec::new());
            }
        }

        // Check for expired health check delays and send pings
        let ready_peers = self.gossip.poll_health_check_delays(cx);
        for peer_id in ready_peers {
            debug!(%peer_id, "Health check delay expired, sending ping");

            if let Some(overlay) = self.connection_registry.resolve_overlay(&peer_id) {
                if let Some(conn_id) = self.connection_registry.active_connection_id(&overlay) {
                    self.pending_actions.push_back(ToSwarm::NotifyHandler {
                        peer_id,
                        handler: NotifyHandler::One(conn_id),
                        event: Command::Ping { greeting: None },
                    });
                }
            }
        }

        // Check for periodic gossip tick via interval
        let gossip_actions = self.gossip.poll_tick(cx);
        self.execute_gossip_actions(gossip_actions);

        // Check for periodic dial candidate evaluation.
        if self.dial_interval.as_mut().poll_tick(cx).is_ready() {
            // Clean up pending connections stuck waiting for TCP/handshake
            self.cleanup_stale_pending();

            self.routing.evaluate_connections();
            self.dial_candidates();
        }

        if let Some(action) = self.pending_actions.pop_front() {
            return Poll::Ready(action);
        }

        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::routing::KademliaConfig;

    #[test]
    fn test_topology_config() {
        let config = TopologyConfig::new()
            .with_kademlia(KademliaConfig::default().with_low_watermark(3))
            .with_dial_interval(Duration::from_secs(10))
            .with_nat_auto(true);

        assert_eq!(config.dial_interval, Duration::from_secs(10));
        assert_eq!(config.kademlia.low_watermark, 3);
        assert!(config.nat_auto);
    }
}
