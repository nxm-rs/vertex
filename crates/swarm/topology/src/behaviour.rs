//! Network topology behaviour.
//!
//! [`TopologyBehaviour`] is a libp2p `NetworkBehaviour` that manages peer connections
//! using handshake, hive, and pingpong protocols. It translates between the public
//! API ([`TopologyCommand`]/[`TopologyEvent`]) and per-connection handlers.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use tokio::time::Sleep;

use libp2p::{
    Multiaddr, PeerId,
    core::{Endpoint, transport::PortUse},
    swarm::{
        ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, NotifyHandler,
        THandlerInEvent, ToSwarm,
    },
};
use tracing::{debug, warn};
use vertex_net_hive::MAX_BATCH_SIZE;
use vertex_swarm_api::{SwarmIdentity, SwarmNodeTypes};
use vertex_swarm_peer::SwarmPeer;
use vertex_swarm_peermanager::{AddressManager, InternalPeerManager, PeerManager, PeerReadyResult};
use vertex_swarm_primitives::OverlayAddress;

use crate::{
    TopologyCommand, TopologyEvent,
    gossip::{GossipAction, HiveGossipConfig, HiveGossipManager},
    handler::{Command, Event, TopologyConfig, TopologyHandler},
};

/// Callback to get current network depth for gossip decisions.
pub type DepthProvider = Arc<dyn Fn() -> u8 + Send + Sync>;

/// Default delay before sending health check ping after handshake.
pub const DEFAULT_HEALTH_CHECK_DELAY: Duration = Duration::from_millis(500);

/// Pending health check awaiting delay before ping.
struct PendingHealthCheck {
    swarm_peer: SwarmPeer,
    is_full_node: bool,
    delay: Pin<Box<Sleep>>,
}

/// Network topology behaviour for handshake, hive, and pingpong protocols.
pub struct TopologyBehaviour<N: SwarmNodeTypes> {
    config: TopologyConfig,
    identity: N::Identity,
    peer_manager: Arc<PeerManager>,
    address_manager: Option<Arc<AddressManager>>,
    peer_connections: HashMap<PeerId, Vec<ConnectionId>>,
    pending_events: VecDeque<TopologyEvent>,
    pending_actions: VecDeque<ToSwarm<TopologyEvent, Command>>,
    gossip_manager: Option<HiveGossipManager>,
    depth_provider: Option<DepthProvider>,
    /// Pending dial intents by address - `true` means for_gossip (delayed ping).
    dial_intents: HashMap<Multiaddr, bool>,
    /// Peers dialed for gossip exchange - get delayed ping.
    gossip_dial_peers: HashSet<PeerId>,
    /// Pending health checks awaiting delay before sending ping.
    pending_health_checks: HashMap<PeerId, PendingHealthCheck>,
    /// Pending gossip waiting for pong response after health check ping.
    pending_gossip: HashMap<PeerId, (SwarmPeer, bool)>,
    /// Delay before sending health check ping after handshake.
    health_check_delay: Duration,
}

impl<N: SwarmNodeTypes> TopologyBehaviour<N> {
    /// Create a new topology behaviour.
    pub fn new(
        identity: N::Identity,
        config: TopologyConfig,
        peer_manager: Arc<PeerManager>,
    ) -> Self {
        Self {
            config,
            identity,
            peer_manager,
            address_manager: None,
            peer_connections: HashMap::new(),
            pending_events: VecDeque::new(),
            pending_actions: VecDeque::new(),
            gossip_manager: None,
            depth_provider: None,
            dial_intents: HashMap::new(),
            gossip_dial_peers: HashSet::new(),
            pending_health_checks: HashMap::new(),
            pending_gossip: HashMap::new(),
            health_check_delay: DEFAULT_HEALTH_CHECK_DELAY,
        }
    }

    /// Create a topology behaviour with address management.
    pub fn with_address_manager(
        identity: N::Identity,
        config: TopologyConfig,
        peer_manager: Arc<PeerManager>,
        address_manager: Arc<AddressManager>,
    ) -> Self {
        Self {
            config,
            identity,
            peer_manager,
            address_manager: Some(address_manager),
            peer_connections: HashMap::new(),
            pending_events: VecDeque::new(),
            pending_actions: VecDeque::new(),
            gossip_manager: None,
            depth_provider: None,
            dial_intents: HashMap::new(),
            gossip_dial_peers: HashSet::new(),
            pending_health_checks: HashMap::new(),
            pending_gossip: HashMap::new(),
            health_check_delay: DEFAULT_HEALTH_CHECK_DELAY,
        }
    }

    /// Set the delay before sending health check ping after handshake.
    pub fn set_health_check_delay(&mut self, delay: Duration) {
        self.health_check_delay = delay;
    }

    /// Enable automatic hive gossip with the given configuration.
    pub fn enable_gossip(
        &mut self,
        gossip_config: HiveGossipConfig,
        depth_provider: DepthProvider,
    ) {
        let local_overlay = self.identity.overlay_address();
        self.gossip_manager = Some(HiveGossipManager::new(
            gossip_config,
            local_overlay,
            self.peer_manager.clone(),
        ));
        self.depth_provider = Some(depth_provider);
    }

    /// Send a ping to a peer by overlay address.
    pub fn ping(&mut self, overlay: &OverlayAddress) {
        let Some(peer_id) = self.peer_manager.resolve_peer_id(overlay) else {
            warn!(%overlay, "Cannot ping: peer not found");
            return;
        };
        if let Some(connections) = self.peer_connections.get(&peer_id)
            && let Some(&connection_id) = connections.first()
        {
            self.pending_actions.push_back(ToSwarm::NotifyHandler {
                peer_id,
                handler: NotifyHandler::One(connection_id),
                event: Command::Ping { greeting: None },
            });
        }
    }

    /// Handle a topology command.
    pub fn on_command(&mut self, command: TopologyCommand) {
        match command {
            TopologyCommand::Dial { addr, for_gossip } => {
                // Store the dial intent - will be matched when connection establishes
                self.dial_intents.insert(addr.clone(), for_gossip);
                debug!(%addr, %for_gossip, "Dial command - dial should be handled by swarm");
            }
            TopologyCommand::CloseConnection(overlay) => {
                let Some(peer_id) = self.peer_manager.resolve_peer_id(&overlay) else {
                    warn!(%overlay, "Cannot close connection: peer not found");
                    return;
                };
                debug!(%overlay, %peer_id, "Close connection command");
                self.pending_actions.push_back(ToSwarm::CloseConnection {
                    peer_id,
                    connection: libp2p::swarm::CloseConnection::All,
                });
            }
        }
    }

    /// Broadcast peers to a specific overlay address via hive protocol.
    ///
    /// Peers are batched into chunks of MAX_BATCH_SIZE for the hive protocol.
    fn broadcast_peers(&mut self, to: OverlayAddress, peers: Vec<SwarmPeer>) {
        let Some(peer_id) = self.peer_manager.resolve_peer_id(&to) else {
            warn!(%to, "Cannot broadcast: peer not found");
            return;
        };
        if let Some(connections) = self.peer_connections.get(&peer_id)
            && let Some(&connection_id) = connections.first()
        {
            for chunk in peers.chunks(MAX_BATCH_SIZE) {
                self.pending_actions.push_back(ToSwarm::NotifyHandler {
                    peer_id,
                    handler: NotifyHandler::One(connection_id),
                    event: Command::BroadcastPeers(chunk.to_vec()),
                });
            }
        }
    }

    /// Execute gossip actions by sending peers via hive protocol.
    fn execute_gossip_actions(&mut self, actions: Vec<GossipAction>) {
        for action in actions {
            self.broadcast_peers(action.to, action.peers);
        }
    }

    /// Get current depth from provider.
    fn current_depth(&self) -> u8 {
        self.depth_provider.as_ref().map(|p| p()).unwrap_or(0)
    }

    /// Check if connected to a peer by overlay address.
    pub fn is_connected(&self, overlay: &OverlayAddress) -> bool {
        self.peer_manager
            .resolve_peer_id(overlay)
            .and_then(|peer_id| self.peer_connections.get(&peer_id))
            .map(|conns| !conns.is_empty())
            .unwrap_or(false)
    }

    /// Get connected overlay addresses.
    pub fn connected_peers(&self) -> Vec<OverlayAddress> {
        self.peer_manager.manager.connected_peers()
    }

    fn process_handler_event(
        &mut self,
        peer_id: PeerId,
        _connection_id: ConnectionId,
        event: Event,
    ) {
        match event {
            Event::HandshakeCompleted(info) => {
                let overlay = OverlayAddress::from(*info.swarm_peer.overlay());
                let is_full_node = info.full_node;
                debug!(%peer_id, %overlay, %is_full_node, "Handshake completed");

                // Register the peer - may replace existing connection
                let result = self
                    .peer_manager
                    .on_peer_ready(peer_id, overlay, is_full_node);

                match result {
                    PeerReadyResult::Replaced { old_peer_id } => {
                        // Close the old connection - new one takes over
                        debug!(
                            %peer_id,
                            %old_peer_id,
                            %overlay,
                            "Closing old connection, new connection takes over"
                        );
                        self.pending_actions.push_back(ToSwarm::CloseConnection {
                            peer_id: old_peer_id,
                            connection: libp2p::swarm::CloseConnection::All,
                        });
                    }
                    PeerReadyResult::DuplicateConnection => {
                        // Same PeerId reconnected - close the NEW connection (keep existing)
                        debug!(
                            %peer_id,
                            %overlay,
                            "Duplicate connection from same peer, closing new connection"
                        );
                        self.pending_actions.push_back(ToSwarm::CloseConnection {
                            peer_id,
                            connection: libp2p::swarm::CloseConnection::All,
                        });
                        return; // Don't emit event or send ping
                    }
                    PeerReadyResult::Accepted => {
                        // Normal new connection - nothing extra to do
                    }
                }

                // Gossip dials get delayed ping; everything else (inbound, kademlia) immediate
                if self.gossip_dial_peers.remove(&peer_id) {
                    // Gossip dial - schedule delayed ping to allow remote to disconnect
                    // first if they intend to (avoids wasted gossip to short-lived connections)
                    let delay = Box::pin(tokio::time::sleep(self.health_check_delay));
                    self.pending_health_checks.insert(
                        peer_id,
                        PendingHealthCheck {
                            swarm_peer: info.swarm_peer.clone(),
                            is_full_node,
                            delay,
                        },
                    );
                    debug!(
                        %peer_id,
                        %overlay,
                        delay_ms = self.health_check_delay.as_millis(),
                        "Scheduled delayed health check ping (gossip dial)"
                    );
                } else {
                    // Inbound or kademlia dial - send ping immediately
                    self.pending_gossip
                        .insert(peer_id, (info.swarm_peer.clone(), is_full_node));

                    if let Some(connections) = self.peer_connections.get(&peer_id)
                        && let Some(&connection_id) = connections.first()
                    {
                        self.pending_actions.push_back(ToSwarm::NotifyHandler {
                            peer_id,
                            handler: NotifyHandler::One(connection_id),
                            event: Command::Ping { greeting: None },
                        });
                        debug!(%peer_id, %overlay, "Sent immediate health check ping");
                    }
                }

                self.pending_events
                    .push_back(TopologyEvent::PeerAuthenticated {
                        peer: info.swarm_peer,
                        is_full_node,
                        welcome_message: info.welcome_message,
                    });
            }
            Event::HandshakeFailed(error) => {
                warn!(%peer_id, %error, "Handshake failed");

                // Record handshake failure in peer stats if we can resolve the overlay
                if let Some(overlay) = self.peer_manager.resolve_overlay(&peer_id) {
                    self.peer_manager.handshake_failed(&overlay);
                    debug!(%overlay, "Recorded handshake failure");
                }

                self.pending_events.push_back(TopologyEvent::DialFailed {
                    address: Multiaddr::empty(),
                    error: error.to_string(),
                });
            }
            Event::HivePeersReceived(peers) => {
                if !peers.is_empty() {
                    let from = self
                        .peer_manager
                        .resolve_overlay(&peer_id)
                        .unwrap_or_else(|| {
                            warn!(%peer_id, "Hive peers from unknown peer");
                            OverlayAddress::default()
                        });
                    debug!(%peer_id, %from, count = peers.len(), "Peers received via hive");
                    self.pending_events
                        .push_back(TopologyEvent::HivePeersReceived { from, peers });
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

                // Record latency in peer manager for QoS
                if let Some(overlay) = self.peer_manager.resolve_overlay(&peer_id) {
                    self.peer_manager.record_latency(&overlay, rtt);
                }

                // Check if we have pending gossip for this peer (from handshake)
                if let Some((swarm_peer, is_full_node)) = self.pending_gossip.remove(&peer_id) {
                    let overlay = OverlayAddress::from(*swarm_peer.overlay());
                    debug!(%peer_id, %overlay, ?rtt, "Connection health verified, triggering gossip");

                    // Now trigger gossip - connection is proven healthy
                    let depth = self.current_depth();
                    let gossip_actions = if let Some(gossip) = &mut self.gossip_manager {
                        let mut actions =
                            gossip.on_peer_authenticated(&swarm_peer, is_full_node, depth);
                        // Check if depth changed due to new peer
                        actions.extend(gossip.check_depth_change(depth));
                        actions
                    } else {
                        Vec::new()
                    };
                    self.execute_gossip_actions(gossip_actions);
                }
            }
            Event::PingpongPingReceived => {
                debug!(%peer_id, "Received ping from peer");
            }
            Event::PingpongError(error) => {
                warn!(%peer_id, %error, "Pingpong failed");

                // Clean up pending gossip if this was a health check ping
                if self.pending_gossip.remove(&peer_id).is_some() {
                    debug!(%peer_id, "Cleaned up pending gossip after ping failure");
                }

                // Record ping timeout in peer stats
                if let Some(overlay) = self.peer_manager.resolve_overlay(&peer_id) {
                    self.peer_manager.connection_timeout(&overlay);
                    debug!(%overlay, "Recorded ping timeout");
                }
            }
        }
    }
}

impl<N: SwarmNodeTypes> NetworkBehaviour for TopologyBehaviour<N> {
    type ConnectionHandler = TopologyHandler<N>;
    type ToSwarm = TopologyEvent;

    fn handle_established_inbound_connection(
        &mut self,
        _connection_id: ConnectionId,
        peer: PeerId,
        _local_addr: &Multiaddr,
        remote_addr: &Multiaddr,
    ) -> Result<Self::ConnectionHandler, ConnectionDenied> {
        let handler = match &self.address_manager {
            Some(mgr) => TopologyHandler::with_address_manager(
                self.config.clone(),
                self.identity.clone(),
                peer,
                remote_addr,
                mgr.clone(),
            ),
            None => TopologyHandler::new(
                self.config.clone(),
                self.identity.clone(),
                peer,
                remote_addr,
            ),
        };
        Ok(handler)
    }

    fn handle_established_outbound_connection(
        &mut self,
        _connection_id: ConnectionId,
        peer: PeerId,
        addr: &Multiaddr,
        _role_override: Endpoint,
        _port_use: PortUse,
    ) -> Result<Self::ConnectionHandler, ConnectionDenied> {
        let handler = match &self.address_manager {
            Some(mgr) => TopologyHandler::with_address_manager(
                self.config.clone(),
                self.identity.clone(),
                peer,
                addr,
                mgr.clone(),
            ),
            None => TopologyHandler::new(self.config.clone(), self.identity.clone(), peer, addr),
        };
        Ok(handler)
    }

    fn on_swarm_event(&mut self, event: FromSwarm) {
        match event {
            FromSwarm::ConnectionEstablished(established) => {
                self.peer_connections
                    .entry(established.peer_id)
                    .or_default()
                    .push(established.connection_id);

                if established.endpoint.is_dialer() {
                    let resolved_addr = established.endpoint.get_remote_address().clone();

                    // Check dial intent - if for_gossip=true, track for delayed ping
                    let for_gossip = self.dial_intents.remove(&resolved_addr).unwrap_or(false);
                    if for_gossip {
                        // Gossip dial - will get delayed ping
                        self.gossip_dial_peers.insert(established.peer_id);
                    }

                    self.pending_actions.push_back(ToSwarm::NotifyHandler {
                        peer_id: established.peer_id,
                        handler: NotifyHandler::One(established.connection_id),
                        event: Command::StartHandshake(resolved_addr),
                    });
                }
            }
            FromSwarm::ConnectionClosed(closed) => {
                if let Some(connections) = self.peer_connections.get_mut(&closed.peer_id) {
                    connections.retain(|&id| id != closed.connection_id);
                }
                if closed.remaining_established == 0 {
                    self.peer_connections.remove(&closed.peer_id);

                    // Clean up gossip dial tracking
                    self.gossip_dial_peers.remove(&closed.peer_id);

                    // Clean up pending health checks (disconnected during delay)
                    if self.pending_health_checks.remove(&closed.peer_id).is_some() {
                        debug!(peer_id = %closed.peer_id, "Cancelled pending health check for disconnected peer");
                    }

                    // Clean up pending gossip for this peer (disconnected before pong)
                    self.pending_gossip.remove(&closed.peer_id);

                    if let Some(overlay) = self.peer_manager.on_peer_disconnected(&closed.peer_id) {
                        debug!(peer_id = %closed.peer_id, %overlay, "Peer disconnected");

                        // Clean up gossip tracking for disconnected peer
                        let depth = self.current_depth();
                        let gossip_actions = if let Some(gossip) = &mut self.gossip_manager {
                            gossip.on_peer_disconnected(&overlay);
                            // Check if depth changed due to disconnection
                            gossip.check_depth_change(depth)
                        } else {
                            Vec::new()
                        };
                        self.execute_gossip_actions(gossip_actions);

                        self.pending_events
                            .push_back(TopologyEvent::PeerConnectionClosed { overlay });
                    }
                }
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
        // Check for expired health check delays and send pings
        let mut ready_peers = Vec::new();
        for (peer_id, check) in &mut self.pending_health_checks {
            if check.delay.as_mut().poll(cx).is_ready() {
                ready_peers.push(*peer_id);
            }
        }
        for peer_id in ready_peers {
            if let Some(check) = self.pending_health_checks.remove(&peer_id) {
                let overlay = OverlayAddress::from(*check.swarm_peer.overlay());
                debug!(%peer_id, %overlay, "Health check delay expired, sending ping");

                // Store peer info for gossip (triggered after pong)
                self.pending_gossip
                    .insert(peer_id, (check.swarm_peer, check.is_full_node));

                // Send ping to verify connection health
                if let Some(connections) = self.peer_connections.get(&peer_id)
                    && let Some(&connection_id) = connections.first()
                {
                    self.pending_actions.push_back(ToSwarm::NotifyHandler {
                        peer_id,
                        handler: NotifyHandler::One(connection_id),
                        event: Command::Ping { greeting: None },
                    });
                }
            }
        }

        // Check for periodic gossip tick
        let depth = self.current_depth();
        let gossip_actions = if let Some(gossip) = &mut self.gossip_manager {
            gossip.maybe_tick(depth)
        } else {
            Vec::new()
        };
        self.execute_gossip_actions(gossip_actions);

        if let Some(event) = self.pending_events.pop_front() {
            return Poll::Ready(ToSwarm::GenerateEvent(event));
        }

        if let Some(action) = self.pending_actions.pop_front() {
            return Poll::Ready(action);
        }

        Poll::Pending
    }
}
