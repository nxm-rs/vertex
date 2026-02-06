//! Network topology behaviour managing peer connections via handshake, hive, and pingpong.

use std::{
    collections::{HashMap, HashSet, VecDeque},
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
    multiaddr::Protocol,
    swarm::{
        ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, NotifyHandler,
        THandlerInEvent, ToSwarm, dial_opts::DialOpts,
    },
};
use tracing::{debug, trace, warn};
use vertex_net_hive::MAX_BATCH_SIZE;
use vertex_swarm_api::{SwarmIdentity, SwarmTopology};
use vertex_swarm_peer::SwarmPeer;
use vertex_swarm_peermanager::{InternalPeerManager, PeerManager, PeerReadyResult};
use vertex_swarm_primitives::OverlayAddress;

use crate::nat_discovery::NatDiscovery;

use crate::{
    TopologyCommand, TopologyServiceEvent,
    gossip::GossipAction,
    gossip_coordinator::{DepthProvider, GossipCoordinator},
    handler::{Command, Event, TopologyConfig, TopologyHandler},
    routing::KademliaRouting,
};

/// Default interval for checking dial candidates.
pub const DEFAULT_DIAL_INTERVAL: Duration = Duration::from_secs(5);

/// Network topology behaviour for handshake, hive, and pingpong protocols.
pub struct TopologyBehaviour<I: SwarmIdentity> {
    config: TopologyConfig,
    identity: Arc<I>,
    peer_manager: Arc<PeerManager>,
    routing: Arc<KademliaRouting<I>>,
    service_event_tx: broadcast::Sender<TopologyServiceEvent>,
    dial_tracker: Arc<crate::dial_tracker::DialTracker>,
    nat_discovery: Arc<NatDiscovery>,
    /// Command receiver for processing dial/disconnect requests from TopologyHandle.
    command_rx: mpsc::Receiver<crate::TopologyCommand>,
    peer_connections: HashMap<PeerId, Vec<ConnectionId>>,
    pending_actions: VecDeque<ToSwarm<(), Command>>,
    /// Gossip coordinator managing health checks and gossip activation.
    gossip_coordinator: GossipCoordinator,
    /// Interval for checking dial candidates from Kademlia routing.
    dial_interval: Pin<Box<Interval>>,
    /// Gossip dials to saturated bins - disconnect after receiving peers.
    gossip_disconnect_pending: HashSet<PeerId>,
}

impl<I: SwarmIdentity> TopologyBehaviour<I> {
    /// Create a new topology behaviour.
    pub fn new(
        identity: I,
        config: TopologyConfig,
        peer_manager: Arc<PeerManager>,
        routing: Arc<KademliaRouting<I>>,
        service_event_tx: broadcast::Sender<TopologyServiceEvent>,
        dial_tracker: Arc<crate::dial_tracker::DialTracker>,
        command_rx: mpsc::Receiver<crate::TopologyCommand>,
        nat_discovery: Arc<NatDiscovery>,
        dial_interval: Option<Duration>,
    ) -> Self {
        // Use provided interval or default (5 seconds)
        let interval_duration = dial_interval.unwrap_or(DEFAULT_DIAL_INTERVAL);

        Self {
            config,
            identity: Arc::new(identity),
            peer_manager,
            routing,
            service_event_tx,
            dial_tracker,
            nat_discovery,
            command_rx,
            peer_connections: HashMap::new(),
            pending_actions: VecDeque::new(),
            gossip_coordinator: GossipCoordinator::new(),
            dial_interval: Box::pin(tokio::time::interval(interval_duration)),
            gossip_disconnect_pending: HashSet::new(),
        }
    }

    /// Set the delay before sending health check ping after handshake.
    pub fn set_health_check_delay(&mut self, delay: Duration) {
        self.gossip_coordinator.set_health_check_delay(delay);
    }

    /// Enable automatic hive gossip.
    pub fn enable_gossip(&mut self, peer_manager: Arc<PeerManager>, depth_provider: DepthProvider) {
        let local_overlay = self.identity.overlay_address();
        self.gossip_coordinator
            .enable_gossip(local_overlay, peer_manager, depth_provider);
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
                // Extract peer_id from multiaddr
                let peer_id = addr.iter().find_map(|p| {
                    if let Protocol::P2p(id) = p {
                        Some(id)
                    } else {
                        None
                    }
                });

                let Some(peer_id) = peer_id else {
                    warn!(%addr, "Dial command missing /p2p/ component in multiaddr");
                    return;
                };

                // Skip if already connected
                if self.peer_connections.contains_key(&peer_id) {
                    debug!(%peer_id, %addr, "Skipping dial command - already connected");
                    return;
                }

                // Track dial intent
                self.dial_tracker.start_dial(vec![addr.clone()], for_gossip);

                // Emit actual dial action
                debug!(%addr, %for_gossip, %peer_id, "Dialing via command");
                let opts = DialOpts::peer_id(peer_id)
                    .addresses(vec![addr])
                    .build();
                self.pending_actions.push_back(ToSwarm::Dial { opts });
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

    /// Dial connection candidates from Kademlia routing.
    fn dial_candidates(&mut self) {
        let candidates = self.routing.peers_to_connect();
        if candidates.is_empty() {
            return;
        }

        // Get dialable SwarmPeers (disconnected peers with known addresses)
        let dialable = self.peer_manager.get_dialable_peers(&candidates);
        let filtered_count = candidates.len() - dialable.len();
        if filtered_count > 0 {
            trace!(
                total_candidates = candidates.len(),
                dialable = dialable.len(),
                filtered = filtered_count,
                "Candidates filtered by peer state"
            );
        }

        // Get current capability once for diagnostic logging
        let capability = self.nat_discovery.capability();

        for swarm_peer in dialable {
            let overlay = OverlayAddress::from(*swarm_peer.overlay());
            let multiaddrs = swarm_peer.multiaddrs();
            let original_count = multiaddrs.len();

            // Filter multiaddrs by IP version compatibility.
            // If capability is None (no listen addresses yet), this filters out all addresses.
            let compatible_addrs: Vec<_> = self
                .nat_discovery
                .filter_dialable(multiaddrs)
                .cloned()
                .collect();

            if compatible_addrs.is_empty() {
                trace!(
                    %overlay,
                    addr_count = original_count,
                    ?capability,
                    "No IP-compatible multiaddrs for peer"
                );
                continue;
            }

            // Find peer_id from first addr (all addrs for same peer should have same peer_id)
            let Some(peer_id) = compatible_addrs.iter().find_map(|addr| {
                addr.iter().find_map(|p| {
                    if let Protocol::P2p(id) = p {
                        Some(id)
                    } else {
                        None
                    }
                })
            }) else {
                debug!(%overlay, "No multiaddr with peer_id found");
                continue;
            };

            // Skip if already connected
            if self.peer_connections.contains_key(&peer_id) {
                trace!(%overlay, %peer_id, "Skipping dial - already connected");
                continue;
            }

            // Check if bin is at capacity before dialing (conservative: assume full node)
            if !self.routing.should_accept_peer(&overlay, true) {
                trace!(
                    %overlay,
                    "Skipping dial - bin at capacity"
                );
                continue;
            }

            // Track dial with all compatible addresses - returns first addr to try
            let Some(addr) = self.dial_tracker.start_dial(compatible_addrs.clone(), false) else {
                trace!(%overlay, "Skipping dial - already dialing this address");
                continue;
            };

            debug!(
                %overlay,
                %addr,
                %peer_id,
                total_addrs = compatible_addrs.len(),
                "Dialing discovered peer"
            );

            // Mark as pending in routing so it won't be selected again
            self.routing.mark_pending_dial(overlay);

            // Emit dial action
            let dial_opts = DialOpts::peer_id(peer_id)
                .addresses(vec![addr])
                .build();
            self.pending_actions.push_back(ToSwarm::Dial { opts: dial_opts });
        }
    }

    /// Try the next multiaddr for a failed dial, or mark as fully failed.
    ///
    /// Returns true if another address will be tried, false if all addresses exhausted.
    fn try_next_dial_addr(&mut self, current_addr: &Multiaddr, overlay: Option<&OverlayAddress>) -> bool {
        if let Some(next_addr) = self.dial_tracker.try_next_addr(current_addr) {
            // Find peer_id from the address
            let peer_id = next_addr.iter().find_map(|p| {
                if let Protocol::P2p(id) = p {
                    Some(id)
                } else {
                    None
                }
            });

            if let Some(peer_id) = peer_id {
                debug!(?overlay, %next_addr, "Trying next multiaddr for peer");
                let dial_opts = DialOpts::peer_id(peer_id)
                    .addresses(vec![next_addr.clone()])
                    .build();
                self.pending_actions.push_back(ToSwarm::Dial { opts: dial_opts });
                return true;
            } else {
                debug!(?overlay, %next_addr, "Next addr has no peer_id, trying another");
                return self.try_next_dial_addr(&next_addr, overlay);
            }
        }

        // No more addresses to try - mark as fully failed
        if let Some(overlay) = overlay {
            self.routing.record_connection_failure(overlay);
            self.routing.clear_pending_dial(overlay);
        }
        false
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
        self.peer_manager.connected_peers()
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

                // Check if this was a gossip dial (before completing tracking)
                let dial_info = self.dial_tracker.get_by_peer_id(&peer_id);
                let is_gossip_dial = dial_info.as_ref().map(|i| i.for_gossip).unwrap_or(false);

                // Complete dial tracking
                self.dial_tracker.complete_dial_by_peer_id(&peer_id);
                self.routing.clear_pending_dial(&overlay);

                // Check if this bin is already saturated (reject overflow connections)
                let bin_at_capacity = !self.routing.should_accept_peer(&overlay, is_full_node);
                if bin_at_capacity {
                    if is_gossip_dial {
                        // Gossip dial to saturated bin: allow for peer exchange, then disconnect
                        debug!(
                            %peer_id,
                            %overlay,
                            "Gossip dial to saturated bin - will disconnect after peer exchange"
                        );
                        self.gossip_disconnect_pending.insert(peer_id);
                    } else {
                        // Non-gossip connection to saturated bin: reject immediately
                        debug!(
                            %peer_id,
                            %overlay,
                            %is_full_node,
                            "Rejecting connection: bin saturated"
                        );
                        self.pending_actions.push_back(ToSwarm::CloseConnection {
                            peer_id,
                            connection: libp2p::swarm::CloseConnection::All,
                        });
                        return;
                    }
                }

                // Register the peer - may replace existing connection
                let result = self
                    .peer_manager
                    .on_peer_ready(peer_id, info.swarm_peer.clone(), is_full_node);

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

                // Update Kademlia routing
                let old_depth = self.routing.depth();
                self.routing.connected(overlay);
                let new_depth = self.routing.depth();

                // Emit depth change event if depth changed
                if new_depth != old_depth {
                    let _ = self.service_event_tx.send(TopologyServiceEvent::DepthChanged {
                        old_depth,
                        new_depth,
                    });
                }

                // Emit PeerReady service event
                let _ = self.service_event_tx.send(TopologyServiceEvent::PeerReady {
                    overlay,
                    peer_id,
                    is_full_node,
                });

                // Delegate gossip coordination - returns action if immediate ping needed
                if let Some(crate::gossip_coordinator::CoordinatorAction::SendPing(ping_peer_id)) =
                    self.gossip_coordinator.on_handshake_completed(
                        peer_id,
                        info.swarm_peer,
                        is_full_node,
                    )
                {
                    // Send immediate health check ping
                    if let Some(connections) = self.peer_connections.get(&ping_peer_id)
                        && let Some(&connection_id) = connections.first()
                    {
                        self.pending_actions.push_back(ToSwarm::NotifyHandler {
                            peer_id: ping_peer_id,
                            handler: NotifyHandler::One(connection_id),
                            event: Command::Ping { greeting: None },
                        });
                        debug!(%peer_id, %overlay, "Sent immediate health check ping");
                    }
                } else {
                    debug!(%peer_id, %overlay, "Scheduled delayed health check ping (gossip dial)");
                }
            }
            Event::HandshakeFailed(error) => {
                warn!(%peer_id, %error, "Handshake failed");

                // Try to resolve overlay via peer_manager, or get dial info to find address
                // Note: HandshakeFailed means the peer was reachable but protocol
                // negotiation failed - trying another address won't help, so we
                // fail the dial entirely (no multi-addr retry).
                let overlay = self.peer_manager.resolve_overlay(&peer_id);
                let dial_info = self.dial_tracker.get_by_peer_id(&peer_id);

                // Complete dial tracking
                self.dial_tracker.complete_dial_by_peer_id(&peer_id);

                if let Some(overlay) = overlay {
                    self.routing.record_connection_failure(&overlay);
                    self.routing.clear_pending_dial(&overlay);
                    debug!(%overlay, "Recorded handshake failure");
                }

                // Emit dial failed service event
                let _ = self.service_event_tx.send(TopologyServiceEvent::DialFailed {
                    addr: dial_info.map(|i| i.addr).unwrap_or_else(Multiaddr::empty),
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

                    // Store dialable peers and get their overlays for Kademlia
                    let stored_overlays = self.peer_manager.store_discovered_peers(peers);

                    if !stored_overlays.is_empty() {
                        self.routing.add_peers(&stored_overlays);
                        self.routing.evaluate_connections();
                        // Dial candidates immediately after evaluation
                        self.dial_candidates();
                    }
                }

                // Disconnect gossip-at-capacity peers after receiving their peer list
                if self.gossip_disconnect_pending.remove(&peer_id) {
                    debug!(
                        %peer_id,
                        "Disconnecting gossip peer after peer exchange (bin at capacity)"
                    );
                    self.pending_actions.push_back(ToSwarm::CloseConnection {
                        peer_id,
                        connection: libp2p::swarm::CloseConnection::All,
                    });
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
                    debug!(%peer_id, %overlay, ?rtt, "Connection health verified, triggering gossip");
                }

                // Delegate to gossip coordinator - returns gossip actions if pending
                let gossip_actions = self.gossip_coordinator.on_pong_received(peer_id);
                self.execute_gossip_actions(gossip_actions);
            }
            Event::PingpongPingReceived => {
                debug!(%peer_id, "Received ping from peer");
            }
            Event::PingpongError(error) => {
                warn!(%peer_id, %error, "Pingpong failed");

                // Clean up pending gossip if this was a health check ping
                if self.gossip_coordinator.on_ping_error(&peer_id) {
                    debug!(%peer_id, "Cleaned up pending gossip after ping failure");
                }
            }
        }
    }
}

impl<I: SwarmIdentity> NetworkBehaviour for TopologyBehaviour<I> {
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
            self.config.clone(),
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
            self.config.clone(),
            self.identity.clone(),
            peer,
            addr,
            self.nat_discovery.clone(),
        ))
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
                    let for_gossip = self
                        .dial_tracker
                        .get(&resolved_addr)
                        .map(|info| info.for_gossip)
                        .unwrap_or(false);
                    if for_gossip {
                        // Gossip dial - will get delayed ping
                        self.gossip_coordinator.mark_gossip_dial(established.peer_id);
                    }

                    // Associate peer_id with the dial for later lookup
                    self.dial_tracker.associate_peer_id(&resolved_addr, established.peer_id);

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

                    // Clean up gossip disconnect tracking
                    self.gossip_disconnect_pending.remove(&closed.peer_id);

                    // Get overlay before cleanup (needed for gossip coordinator)
                    let overlay = self.peer_manager.on_peer_disconnected(&closed.peer_id);

                    // Clean up gossip coordinator state and get any resulting actions
                    let gossip_actions = self
                        .gossip_coordinator
                        .on_connection_closed(&closed.peer_id, overlay.as_ref());
                    self.execute_gossip_actions(gossip_actions);

                    if let Some(overlay) = overlay {
                        debug!(peer_id = %closed.peer_id, %overlay, "Peer disconnected");

                        // Update Kademlia routing
                        let old_depth = self.routing.depth();
                        self.routing.disconnected(&overlay);
                        let new_depth = self.routing.depth();

                        // Emit PeerDisconnected service event
                        let _ = self
                            .service_event_tx
                            .send(TopologyServiceEvent::PeerDisconnected { overlay });

                        // Emit depth change event if depth changed
                        if new_depth != old_depth {
                            let _ =
                                self.service_event_tx.send(TopologyServiceEvent::DepthChanged {
                                    old_depth,
                                    new_depth,
                                });
                        }
                    }
                }
            }
            FromSwarm::DialFailure(failure) => {
                // Handle dial failure - try next address if available
                if let Some(peer_id) = failure.peer_id {
                    // Find the current dial address via peer_id
                    if let Some(current_addr) = self.dial_tracker.find_addr_by_peer_id(&peer_id) {
                        // Try to resolve overlay for logging/routing cleanup
                        let overlay = self.peer_manager.resolve_overlay(&peer_id);

                        if self.try_next_dial_addr(&current_addr, overlay.as_ref()) {
                            debug!(
                                %peer_id,
                                ?overlay,
                                "Dial failed, trying next multiaddr"
                            );
                        } else {
                            warn!(
                                %peer_id,
                                ?overlay,
                                "Dial failed (all addresses exhausted)"
                            );
                            // Emit dial failed event
                            let _ = self.service_event_tx.send(TopologyServiceEvent::DialFailed {
                                addr: current_addr,
                                error: format!("All addresses exhausted for {:?}", overlay),
                            });
                        }
                    } else {
                        // Could not find dial info - dial wasn't tracked
                        // Check if we can recover via peer_manager's registry.
                        if let Some(overlay) = self.peer_manager.resolve_overlay(&peer_id) {
                            trace!(
                                %peer_id,
                                %overlay,
                                "DialFailure for untracked dial, resetting peer state via routing"
                            );
                            self.routing.record_connection_failure(&overlay);
                            self.routing.clear_pending_dial(&overlay);
                        } else {
                            trace!(
                                %peer_id,
                                "DialFailure for unknown peer_id (no dial tracking)"
                            );
                        }
                    }
                } else {
                    trace!("DialFailure without peer_id");
                }
            }
            FromSwarm::NewListenAddr(info) => {
                debug!(address = %info.addr, "New listen address");
                let capability_became_known = self.nat_discovery.on_new_listen_addr(info.addr.clone());

                // Trigger immediate dialing when capability first becomes known
                // (we now know our IP version and can filter peer addresses)
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

        // Check for expired health check delays and send pings
        let ready_peers = self.gossip_coordinator.poll_health_check_delays(cx);
        for peer_id in ready_peers {
            debug!(%peer_id, "Health check delay expired, sending ping");

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

        // Check for periodic gossip tick via interval
        let gossip_actions = self.gossip_coordinator.poll_gossip_tick(cx);
        self.execute_gossip_actions(gossip_actions);

        // Check for periodic dial candidate evaluation
        if self.dial_interval.as_mut().poll_tick(cx).is_ready() {
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
    use std::task::{Context, Poll, Waker, RawWaker, RawWakerVTable};

    use alloy_primitives::B256;
    use alloy_signer_local::LocalSigner;
    use libp2p::swarm::ToSwarm;
    use nectar_primitives::SwarmAddress;
    use vertex_swarm_spec::Spec;

    use crate::dial_tracker::DialTracker;
    use crate::routing::KademliaConfig;

    #[derive(Clone)]
    struct MockIdentity {
        overlay: SwarmAddress,
        signer: Arc<LocalSigner<alloy_signer::k256::ecdsa::SigningKey>>,
        spec: Arc<Spec>,
    }

    impl std::fmt::Debug for MockIdentity {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("MockIdentity")
                .field("overlay", &self.overlay)
                .finish_non_exhaustive()
        }
    }

    impl MockIdentity {
        fn with_overlay(overlay: OverlayAddress) -> Self {
            let signer = LocalSigner::random();
            Self {
                overlay,
                signer: Arc::new(signer),
                spec: vertex_swarm_spec::init_testnet(),
            }
        }
    }

    impl SwarmIdentity for MockIdentity {
        type Spec = Spec;
        type Signer = LocalSigner<alloy_signer::k256::ecdsa::SigningKey>;

        fn spec(&self) -> &Self::Spec {
            &self.spec
        }

        fn nonce(&self) -> B256 {
            B256::ZERO
        }

        fn signer(&self) -> Arc<Self::Signer> {
            self.signer.clone()
        }

        fn node_type(&self) -> vertex_swarm_api::SwarmNodeType {
            vertex_swarm_api::SwarmNodeType::Client
        }

        fn overlay_address(&self) -> SwarmAddress {
            self.overlay
        }
    }

    fn addr_from_byte(b: u8) -> OverlayAddress {
        let mut bytes = [0u8; 32];
        bytes[0] = b;
        OverlayAddress::from(bytes)
    }

    fn make_swarm_peer(overlay_byte: u8) -> SwarmPeer {
        use alloy_primitives::{Address, Signature, U256};
        let overlay = addr_from_byte(overlay_byte);
        SwarmPeer::from_validated(
            vec![format!("/ip4/127.0.0.{}/tcp/1634", overlay_byte).parse().unwrap()],
            Signature::new(U256::ZERO, U256::ZERO, false),
            B256::from_slice(overlay.as_slice()),
            B256::ZERO,
            Address::ZERO,
        )
    }

    /// Create a minimal behaviour for testing command handling.
    fn create_test_behaviour() -> (
        TopologyBehaviour<MockIdentity>,
        mpsc::Sender<TopologyCommand>,
    ) {
        use vertex_net_local::LocalCapabilities;
        use crate::nat_discovery::NatDiscovery;

        let base = addr_from_byte(0x00);
        let identity = MockIdentity::with_overlay(base);

        let peer_manager = Arc::new(PeerManager::new());
        let routing = KademliaRouting::new(identity.clone(), KademliaConfig::default());
        let (event_tx, _) = broadcast::channel(16);
        let dial_tracker = Arc::new(DialTracker::new());
        let (command_tx, command_rx) = mpsc::channel(16);
        let local_capabilities = Arc::new(LocalCapabilities::new());
        let nat_discovery = Arc::new(NatDiscovery::disabled(local_capabilities));

        let config = TopologyConfig::default();

        let behaviour = TopologyBehaviour::new(
            identity,
            config,
            peer_manager,
            routing,
            event_tx,
            dial_tracker,
            command_rx,
            nat_discovery,
            Some(Duration::from_secs(60)), // Long interval to avoid interference
        );

        (behaviour, command_tx)
    }

    /// Create a dummy waker for polling.
    fn dummy_waker() -> Waker {
        fn raw_waker() -> RawWaker {
            fn no_op(_: *const ()) {}
            fn clone(_: *const ()) -> RawWaker { raw_waker() }
            static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, no_op, no_op, no_op);
            RawWaker::new(std::ptr::null(), &VTABLE)
        }
        unsafe { Waker::from_raw(raw_waker()) }
    }

    #[tokio::test]
    async fn test_dial_command_with_valid_multiaddr_emits_dial_action() {
        let (mut behaviour, _tx) = create_test_behaviour();

        // Create a valid multiaddr with /p2p/ component
        let peer_id = PeerId::random();
        let addr: Multiaddr = format!("/ip4/127.0.0.1/tcp/9000/p2p/{}", peer_id)
            .parse()
            .unwrap();

        // Send dial command directly
        behaviour.on_command(TopologyCommand::Dial {
            addr: addr.clone(),
            for_gossip: false,
        });

        // Check that a dial action was queued
        assert_eq!(behaviour.pending_actions.len(), 1);

        let action = behaviour.pending_actions.pop_front().unwrap();
        match action {
            ToSwarm::Dial { opts } => {
                // Verify the dial opts contain the right peer_id
                assert_eq!(opts.get_peer_id(), Some(peer_id));
            }
            other => panic!("Expected ToSwarm::Dial, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_dial_command_without_peer_id_does_not_emit_dial() {
        let (mut behaviour, _tx) = create_test_behaviour();

        // Create a multiaddr WITHOUT /p2p/ component
        let addr: Multiaddr = "/ip4/127.0.0.1/tcp/9000".parse().unwrap();

        // Send dial command
        behaviour.on_command(TopologyCommand::Dial {
            addr,
            for_gossip: false,
        });

        // Should NOT queue any dial action (missing peer_id)
        assert!(
            behaviour.pending_actions.is_empty(),
            "Expected no actions, but found: {:?}",
            behaviour.pending_actions
        );
    }

    #[tokio::test]
    async fn test_dial_command_for_already_connected_peer_does_not_emit_dial() {
        let (mut behaviour, _tx) = create_test_behaviour();

        let peer_id = PeerId::random();
        let addr: Multiaddr = format!("/ip4/127.0.0.1/tcp/9000/p2p/{}", peer_id)
            .parse()
            .unwrap();

        // Simulate that the peer is already connected
        behaviour
            .peer_connections
            .insert(peer_id, vec![ConnectionId::new_unchecked(1)]);

        // Send dial command
        behaviour.on_command(TopologyCommand::Dial {
            addr,
            for_gossip: false,
        });

        // Should NOT queue dial action (already connected)
        assert!(
            behaviour.pending_actions.is_empty(),
            "Expected no actions for already-connected peer, but found: {:?}",
            behaviour.pending_actions
        );
    }

    #[tokio::test]
    async fn test_dial_command_with_gossip_flag_tracks_correctly() {
        let (mut behaviour, _tx) = create_test_behaviour();

        let peer_id = PeerId::random();
        let addr: Multiaddr = format!("/ip4/127.0.0.1/tcp/9000/p2p/{}", peer_id)
            .parse()
            .unwrap();

        // Send dial command with for_gossip=true
        behaviour.on_command(TopologyCommand::Dial {
            addr: addr.clone(),
            for_gossip: true,
        });

        // Verify dial action was queued
        assert_eq!(behaviour.pending_actions.len(), 1);

        // Verify dial tracker has the for_gossip flag
        let dial_info = behaviour.dial_tracker.get(&addr);
        assert!(dial_info.is_some());
        assert!(dial_info.unwrap().for_gossip);
    }

    #[tokio::test]
    async fn test_dial_action_returned_from_poll() {
        let (mut behaviour, _tx) = create_test_behaviour();

        let peer_id = PeerId::random();
        let addr: Multiaddr = format!("/ip4/127.0.0.1/tcp/9000/p2p/{}", peer_id)
            .parse()
            .unwrap();

        // Queue a dial command
        behaviour.on_command(TopologyCommand::Dial {
            addr,
            for_gossip: false,
        });

        // Poll the behaviour
        let waker = dummy_waker();
        let mut cx = Context::from_waker(&waker);

        let result = behaviour.poll(&mut cx);

        // Should return the dial action
        match result {
            Poll::Ready(ToSwarm::Dial { opts }) => {
                assert_eq!(opts.get_peer_id(), Some(peer_id));
            }
            other => panic!("Expected Poll::Ready(ToSwarm::Dial), got {:?}", other),
        }

        // Queue should be empty now
        assert!(behaviour.pending_actions.is_empty());
    }

    #[tokio::test]
    async fn test_close_connection_command() {
        let (mut behaviour, _tx) = create_test_behaviour();

        let peer_id = PeerId::random();
        let swarm_peer = make_swarm_peer(0x80);
        let overlay = OverlayAddress::from(*swarm_peer.overlay());

        // Register the peer in peer_manager so it can be resolved
        behaviour
            .peer_manager
            .on_peer_ready(peer_id, swarm_peer, false);
        behaviour
            .peer_connections
            .insert(peer_id, vec![ConnectionId::new_unchecked(1)]);

        // Send close command
        behaviour.on_command(TopologyCommand::CloseConnection(overlay));

        // Should queue a close connection action
        assert_eq!(behaviour.pending_actions.len(), 1);

        let action = behaviour.pending_actions.pop_front().unwrap();
        match action {
            ToSwarm::CloseConnection {
                peer_id: closed_peer,
                ..
            } => {
                assert_eq!(closed_peer, peer_id);
            }
            other => panic!("Expected ToSwarm::CloseConnection, got {:?}", other),
        }
    }
}
