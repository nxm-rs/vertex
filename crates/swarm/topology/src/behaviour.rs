//! Network topology behaviour managing peer connections via handshake, hive, and pingpong.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    future::Future,
    path::PathBuf,
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
use tracing::{debug, info, trace, warn};
use vertex_net_hive::MAX_BATCH_SIZE;
use vertex_net_local::LocalCapabilities;
use vertex_swarm_api::{PeerConfigValues, SwarmBootnodeConfig, SwarmIdentity, SwarmTopology};
use vertex_swarm_peer::SwarmPeer;
use vertex_swarm_peermanager::{FilePeerStore, InternalPeerManager, PeerManager, PeerReadyResult};
use vertex_swarm_primitives::OverlayAddress;

use crate::bootnode::BootnodeConnector;
use crate::dial_tracker::DialTracker;
use crate::dns::{is_dnsaddr, resolve_all_dnsaddrs};
use crate::error::TopologyError;
use crate::events::TopologyServiceEvent;
use crate::gossip::GossipAction;
use crate::gossip_coordinator::{DepthProvider, GossipCoordinator};
use crate::handle::TopologyHandle;
use crate::handler::{Command, Event, TopologyConfig, TopologyHandler};
use crate::nat_discovery::{NatDiscovery, NatDiscoveryConfig};
use crate::routing::{KademliaConfig, KademliaRouting, PeerFailureProvider, SwarmRouting};
use crate::TopologyCommand;

pub const DEFAULT_DIAL_INTERVAL: Duration = Duration::from_secs(5);

const EVENT_CHANNEL_CAPACITY: usize = 256;
const COMMAND_CHANNEL_CAPACITY: usize = 64;

/// Configuration for TopologyBehaviour construction.
#[derive(Debug, Clone, Default)]
pub struct TopologyBehaviourConfig {
    pub kademlia: KademliaConfig,
    pub dial_interval: Option<Duration>,
    pub nat: NatDiscoveryConfig,
    pub nat_auto: bool,
}

impl TopologyBehaviourConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_kademlia(mut self, config: KademliaConfig) -> Self {
        self.kademlia = config;
        self
    }

    pub fn with_dial_interval(mut self, interval: Duration) -> Self {
        self.dial_interval = Some(interval);
        self
    }

    pub fn with_nat_auto(mut self, enabled: bool) -> Self {
        self.nat_auto = enabled;
        self
    }
}

/// Implement PeerFailureProvider for PeerManager to delegate failure tracking.
impl PeerFailureProvider for PeerManager {
    fn failure_score(&self, peer: &OverlayAddress) -> f64 {
        self.peer_score(peer)
    }

    fn record_failure(&self, peer: &OverlayAddress) {
        self.adjust_score(peer, -1.0);
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

    // Owned (internal only, Arc for handler sharing)
    dial_tracker: DialTracker,
    nat_discovery: Arc<NatDiscovery>,
    local_capabilities: Arc<LocalCapabilities>,
    bootnode_connector: BootnodeConnector,
    trusted_peers: Vec<Multiaddr>,

    // Channels
    command_rx: mpsc::Receiver<TopologyCommand>,
    event_tx: broadcast::Sender<TopologyServiceEvent>,

    // Connection state
    peer_connections: HashMap<PeerId, Vec<ConnectionId>>,
    pending_actions: VecDeque<ToSwarm<(), Command>>,

    // Gossip coordination
    gossip_coordinator: GossipCoordinator,

    // Timers
    dial_interval: Pin<Box<Interval>>,

    // State flags
    gossip_disconnect_pending: HashSet<PeerId>,

    // Pending dnsaddr resolution for bootnodes
    pending_bootnode_resolution: Option<Pin<Box<dyn Future<Output = Vec<Multiaddr>> + Send>>>,
}

impl<I: SwarmIdentity> TopologyBehaviour<I> {
    pub fn has_bootnodes(&self) -> bool {
        self.bootnode_connector.has_bootnodes()
    }

    pub fn save_peers(&self) -> Result<usize, String> {
        self.peer_manager
            .save_all_to_store()
            .map_err(|e| e.to_string())
    }

    pub fn set_health_check_delay(&mut self, delay: Duration) {
        self.gossip_coordinator.set_health_check_delay(delay);
    }

    pub fn is_connected(&self, overlay: &OverlayAddress) -> bool {
        self.peer_manager
            .resolve_peer_id(overlay)
            .and_then(|peer_id| self.peer_connections.get(&peer_id))
            .map(|conns| !conns.is_empty())
            .unwrap_or(false)
    }

    pub fn connected_peers(&self) -> Vec<OverlayAddress> {
        self.peer_manager.connected_peers()
    }
}

impl<I: SwarmIdentity + Clone> TopologyBehaviour<I> {
    /// Create topology behaviour and handle.
    pub fn new(
        identity: I,
        handler_config: TopologyConfig,
        behaviour_config: TopologyBehaviourConfig,
        network_config: &impl SwarmBootnodeConfig,
    ) -> Result<(Self, TopologyHandle<I>), TopologyError> {
        let bootnodes = network_config.bootnodes().to_vec();
        let trusted_peers = network_config.trusted_peers().to_vec();
        let nat_addrs = network_config.nat_addrs().to_vec();
        let nat_auto = network_config.nat_auto_enabled() || behaviour_config.nat_auto;
        let peer_store_path = network_config.peers().store_path();
        let peer_ban_threshold = network_config.peers().ban_threshold();
        let peer_store_limit = network_config.peers().store_limit();

        let (event_tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let (command_tx, command_rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);

        let peer_manager = Self::create_peer_manager(
            peer_store_path.as_ref(),
            peer_ban_threshold,
            peer_store_limit,
        )?;

        let routing = KademliaRouting::with_failure_provider(
            identity.clone(),
            behaviour_config.kademlia,
            peer_manager.clone(),
        );

        let known_peers = peer_manager.disconnected_peers();
        if !known_peers.is_empty() {
            info!(count = known_peers.len(), "seeding kademlia with stored peers");
            routing.add_peers(&known_peers);
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
                behaviour_config.nat,
                nat_auto,
            )
        } else {
            NatDiscovery::disabled(local_capabilities.clone())
        });

        let bootnode_connector = BootnodeConnector::new(bootnodes);
        let dial_tracker = DialTracker::new();
        let mut gossip_coordinator = GossipCoordinator::new();

        let depth_provider: DepthProvider = {
            let routing_clone = routing.clone();
            Arc::new(move || routing_clone.depth())
        };

        gossip_coordinator.enable_gossip(
            identity.overlay_address(),
            peer_manager.clone(),
            depth_provider,
        );

        let handle = TopologyHandle::new(
            routing.clone(),
            peer_manager.clone(),
            command_tx,
            event_tx.clone(),
        );

        let interval_duration = behaviour_config.dial_interval.unwrap_or(DEFAULT_DIAL_INTERVAL);

        let behaviour = Self {
            config: handler_config,
            identity: Arc::new(identity),
            routing,
            peer_manager,
            dial_tracker,
            nat_discovery,
            local_capabilities,
            bootnode_connector,
            trusted_peers,
            command_rx,
            event_tx,
            peer_connections: HashMap::new(),
            pending_actions: VecDeque::new(),
            gossip_coordinator,
            dial_interval: Box::pin(tokio::time::interval(interval_duration)),
            gossip_disconnect_pending: HashSet::new(),
            pending_bootnode_resolution: None,
        };

        Ok((behaviour, handle))
    }

    fn create_peer_manager(
        store_path: Option<&PathBuf>,
        ban_threshold: f64,
        max_peers: Option<usize>,
    ) -> Result<Arc<PeerManager>, TopologyError> {
        match store_path {
            Some(path) => {
                let store = FilePeerStore::new_with_create_dir(path).map_err(|e| {
                    TopologyError::PeerStoreCreation {
                        path: path.clone(),
                        reason: e.to_string(),
                    }
                })?;

                match PeerManager::with_store_and_limits(Arc::new(store), ban_threshold, max_peers) {
                    Ok(pm) => {
                        info!(count = pm.stats().total_peers, path = %path.display(), "loaded peers from store");
                        Ok(Arc::new(pm))
                    }
                    Err(e) => Err(TopologyError::PeerStoreLoad {
                        reason: e.to_string(),
                    }),
                }
            }
            None => Ok(Arc::new(PeerManager::with_limits(ban_threshold, max_peers))),
        }
    }

    fn connect_bootnodes(&mut self) {
        let bootnodes = self.bootnode_connector.shuffled_bootnodes();
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
        if !bootnodes.is_empty() {
            info!(count = bootnodes.len(), "Connecting to bootnodes...");

            let min_connections = self.bootnode_connector.min_connections();
            let mut connected = 0;

            for bootnode in bootnodes {
                if connected >= min_connections {
                    info!(connected, "Reached minimum bootnode connections");
                    break;
                }

                let peer_id = bootnode.iter().find_map(|p| {
                    if let Protocol::P2p(id) = p {
                        Some(id)
                    } else {
                        None
                    }
                });

                let Some(peer_id) = peer_id else {
                    debug!(%bootnode, "Bootnode missing /p2p/ component, skipping");
                    continue;
                };

                debug!(%bootnode, %peer_id, "Dialing bootnode");
                let opts = DialOpts::peer_id(peer_id)
                    .addresses(vec![bootnode])
                    .build();
                self.pending_actions.push_back(ToSwarm::Dial { opts });
                connected += 1;
            }
        }

        // Connect trusted peers
        for peer in trusted_peers {
            let peer_id = peer.iter().find_map(|p| {
                if let Protocol::P2p(id) = p {
                    Some(id)
                } else {
                    None
                }
            });

            if let Some(peer_id) = peer_id {
                debug!(%peer, %peer_id, "Dialing trusted peer");
                let opts = DialOpts::peer_id(peer_id)
                    .addresses(vec![peer.clone()])
                    .build();
                self.pending_actions.push_back(ToSwarm::Dial { opts });
            }
        }
    }

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

    /// Handle a topology command (dial, close connection, etc.).
    pub fn on_command(&mut self, command: TopologyCommand) {
        match command {
            TopologyCommand::ConnectBootnodes => {
                self.connect_bootnodes();
            }
            TopologyCommand::Dial { addr, for_gossip } => {
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

                if self.peer_connections.contains_key(&peer_id) {
                    debug!(%peer_id, %addr, "Skipping dial command - already connected");
                    return;
                }

                self.dial_tracker.start_dial(vec![addr.clone()], for_gossip);

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

        let capability = self.nat_discovery.capability();

        for swarm_peer in dialable {
            let overlay = OverlayAddress::from(*swarm_peer.overlay());
            let multiaddrs = swarm_peer.multiaddrs();
            let original_count = multiaddrs.len();

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

            if self.peer_connections.contains_key(&peer_id) {
                trace!(%overlay, %peer_id, "Skipping dial - already connected");
                continue;
            }

            if !self.routing.should_accept_peer(&overlay, true) {
                trace!(%overlay, "Skipping dial - bin at capacity");
                continue;
            }

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

            self.routing.mark_pending_dial(overlay);

            let dial_opts = DialOpts::peer_id(peer_id)
                .addresses(vec![addr])
                .build();
            self.pending_actions.push_back(ToSwarm::Dial { opts: dial_opts });
        }
    }

    fn try_next_dial_addr(&mut self, current_addr: &Multiaddr, overlay: Option<&OverlayAddress>) -> bool {
        if let Some(next_addr) = self.dial_tracker.try_next_addr(current_addr) {
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

        if let Some(overlay) = overlay {
            self.routing.record_connection_failure(overlay);
            self.routing.clear_pending_dial(overlay);
        }
        false
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

                let dial_info = self.dial_tracker.get_by_peer_id(&peer_id);
                let is_gossip_dial = dial_info.as_ref().map(|i| i.for_gossip).unwrap_or(false);

                self.dial_tracker.complete_dial_by_peer_id(&peer_id);
                self.routing.clear_pending_dial(&overlay);

                let bin_at_capacity = !self.routing.should_accept_peer(&overlay, is_full_node);
                if bin_at_capacity {
                    if is_gossip_dial {
                        debug!(
                            %peer_id,
                            %overlay,
                            "Gossip dial to saturated bin - will disconnect after peer exchange"
                        );
                        self.gossip_disconnect_pending.insert(peer_id);
                    } else {
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

                let result = self
                    .peer_manager
                    .on_peer_ready(peer_id, info.swarm_peer.clone(), is_full_node);

                match result {
                    PeerReadyResult::Replaced { old_peer_id } => {
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
                        debug!(
                            %peer_id,
                            %overlay,
                            "Duplicate connection from same peer, closing new connection"
                        );
                        self.pending_actions.push_back(ToSwarm::CloseConnection {
                            peer_id,
                            connection: libp2p::swarm::CloseConnection::All,
                        });
                        return;
                    }
                    PeerReadyResult::Accepted => {}
                }

                let old_depth = self.routing.depth();
                self.routing.connected(overlay);
                let new_depth = self.routing.depth();

                if new_depth != old_depth {
                    let _ = self.event_tx.send(TopologyServiceEvent::DepthChanged {
                        old_depth,
                        new_depth,
                    });
                }

                let _ = self.event_tx.send(TopologyServiceEvent::PeerReady {
                    overlay,
                    peer_id,
                    is_full_node,
                });

                if let Some(crate::gossip_coordinator::CoordinatorAction::SendPing(ping_peer_id)) =
                    self.gossip_coordinator.on_handshake_completed(
                        peer_id,
                        info.swarm_peer,
                        is_full_node,
                    )
                {
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

                let overlay = self.peer_manager.resolve_overlay(&peer_id);
                let dial_info = self.dial_tracker.get_by_peer_id(&peer_id);

                self.dial_tracker.complete_dial_by_peer_id(&peer_id);

                if let Some(overlay) = overlay {
                    self.routing.record_connection_failure(&overlay);
                    self.routing.clear_pending_dial(&overlay);
                    debug!(%overlay, "Recorded handshake failure");
                }

                let _ = self.event_tx.send(TopologyServiceEvent::DialFailed {
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

                    let stored_overlays = self.peer_manager.store_discovered_peers(peers);

                    if !stored_overlays.is_empty() {
                        self.routing.add_peers(&stored_overlays);
                        self.routing.evaluate_connections();
                        self.dial_candidates();
                    }
                }

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

                if let Some(overlay) = self.peer_manager.resolve_overlay(&peer_id) {
                    self.peer_manager.record_latency(&overlay, rtt);
                    debug!(%peer_id, %overlay, ?rtt, "Connection health verified, triggering gossip");
                }

                let gossip_actions = self.gossip_coordinator.on_pong_received(peer_id);
                self.execute_gossip_actions(gossip_actions);
            }
            Event::PingpongPingReceived => {
                debug!(%peer_id, "Received ping from peer");
            }
            Event::PingpongError(error) => {
                warn!(%peer_id, %error, "Pingpong failed");

                if self.gossip_coordinator.on_ping_error(&peer_id) {
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

                    let for_gossip = self
                        .dial_tracker
                        .get(&resolved_addr)
                        .map(|info| info.for_gossip)
                        .unwrap_or(false);
                    if for_gossip {
                        self.gossip_coordinator.mark_gossip_dial(established.peer_id);
                    }

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

                    self.gossip_disconnect_pending.remove(&closed.peer_id);

                    let overlay = self.peer_manager.on_peer_disconnected(&closed.peer_id);

                    let gossip_actions = self
                        .gossip_coordinator
                        .on_connection_closed(&closed.peer_id, overlay.as_ref());
                    self.execute_gossip_actions(gossip_actions);

                    if let Some(overlay) = overlay {
                        debug!(peer_id = %closed.peer_id, %overlay, "Peer disconnected");

                        let old_depth = self.routing.depth();
                        self.routing.disconnected(&overlay);
                        let new_depth = self.routing.depth();

                        let _ = self
                            .event_tx
                            .send(TopologyServiceEvent::PeerDisconnected { overlay });

                        if new_depth != old_depth {
                            let _ =
                                self.event_tx.send(TopologyServiceEvent::DepthChanged {
                                    old_depth,
                                    new_depth,
                                });
                        }
                    }
                }
            }
            FromSwarm::DialFailure(failure) => {
                if let Some(peer_id) = failure.peer_id {
                    if let Some(current_addr) = self.dial_tracker.find_addr_by_peer_id(&peer_id) {
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
                            let _ = self.event_tx.send(TopologyServiceEvent::DialFailed {
                                addr: current_addr,
                                error: format!("All addresses exhausted for {:?}", overlay),
                            });
                        }
                    } else {
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
        let ready_peers = self.gossip_coordinator.poll_health_check_delays(cx);
        for peer_id in ready_peers {
            debug!(%peer_id, "Health check delay expired, sending ping");

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

    fn dummy_waker() -> Waker {
        fn raw_waker() -> RawWaker {
            fn no_op(_: *const ()) {}
            fn clone(_: *const ()) -> RawWaker { raw_waker() }
            static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, no_op, no_op, no_op);
            RawWaker::new(std::ptr::null(), &VTABLE)
        }
        unsafe { Waker::from_raw(raw_waker()) }
    }

    #[test]
    fn test_behaviour_config() {
        let config = TopologyBehaviourConfig::new()
            .with_kademlia(KademliaConfig::default().with_low_watermark(3))
            .with_dial_interval(Duration::from_secs(10))
            .with_nat_auto(true);

        assert_eq!(config.dial_interval, Some(Duration::from_secs(10)));
        assert_eq!(config.kademlia.low_watermark, 3);
        assert!(config.nat_auto);
    }
}
