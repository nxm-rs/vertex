//! SwarmTopologyBehaviour for network topology management.
//!
//! This behaviour handles peer discovery, authentication, and connection liveness
//! for Swarm nodes. It manages:
//!
//! - Peer authentication via handshake
//! - Peer discovery via hive
//! - Connection liveness via pingpong

use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    task::{Context, Poll},
};

use libp2p::{
    Multiaddr, PeerId,
    core::{Endpoint, transport::PortUse},
    swarm::{
        ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, NotifyHandler,
        THandlerInEvent, ToSwarm,
    },
};
use tracing::{debug, warn};
use vertex_net_handshake::HandshakeInfo;
use vertex_net_hive::{BzzAddress, MAX_BATCH_SIZE, Peers};
use vertex_net_primitives_traits::NodeAddress as NodeAddressTrait;
use vertex_node_types::NodeTypes;
use vertex_primitives::OverlayAddress;

use crate::{
    TopologyCommand, TopologyEvent,
    handler::{
        Command as HandlerCommand, Config as HandlerConfig, Event as HandlerEvent, TopologyHandler,
    },
};

/// Configuration for the topology behaviour.
#[derive(Debug, Clone, Default)]
pub struct Config {
    /// Handler configuration.
    pub handler: HandlerConfig,
}

/// Information about a connected peer.
#[derive(Debug, Clone)]
pub struct PeerInfo {
    /// The peer's overlay address.
    pub overlay: OverlayAddress,
    /// Whether the peer is a full node.
    pub is_full_node: bool,
    /// The handshake info.
    pub handshake: HandshakeInfo,
}

/// SwarmTopologyBehaviour manages network topology.
///
/// This behaviour handles handshake, hive, and pingpong protocols. It maps
/// events from the handler to `TopologyEvent` and handles `TopologyCommand`
/// from the node layer.
///
/// Generic over `N: NodeTypes` to support different node configurations.
pub struct SwarmTopologyBehaviour<N: NodeTypes> {
    /// Configuration.
    config: Config,
    /// Node identity for handshake.
    identity: Arc<N::Identity>,
    /// Map of peer_id -> peer info for authenticated peers.
    authenticated_peers: HashMap<PeerId, PeerInfo>,
    /// Map of overlay -> peer_id for reverse lookup.
    overlay_to_peer: HashMap<OverlayAddress, PeerId>,
    /// Map of peer_id -> connection IDs.
    peer_connections: HashMap<PeerId, Vec<ConnectionId>>,
    /// Cache of overlay -> underlays for dialing discovered peers.
    underlay_cache: HashMap<OverlayAddress, Vec<Multiaddr>>,
    /// Pending topology events to emit.
    pending_events: VecDeque<TopologyEvent>,
    /// Pending swarm actions.
    pending_actions: VecDeque<ToSwarm<TopologyEvent, HandlerCommand>>,
}

impl<N: NodeTypes> SwarmTopologyBehaviour<N> {
    /// Create a new topology behaviour.
    pub fn new(identity: Arc<N::Identity>, config: Config) -> Self {
        Self {
            config,
            identity,
            authenticated_peers: HashMap::new(),
            overlay_to_peer: HashMap::new(),
            peer_connections: HashMap::new(),
            underlay_cache: HashMap::new(),
            pending_events: VecDeque::new(),
            pending_actions: VecDeque::new(),
        }
    }

    /// Send a ping to a peer and measure RTT.
    pub fn ping(&mut self, peer_id: PeerId) {
        if let Some(connections) = self.peer_connections.get(&peer_id) {
            if let Some(&connection_id) = connections.first() {
                self.pending_actions.push_back(ToSwarm::NotifyHandler {
                    peer_id,
                    handler: NotifyHandler::One(connection_id),
                    event: HandlerCommand::Ping { greeting: None },
                });
            }
        }
    }

    /// Handle a command from the node layer.
    pub fn on_command(&mut self, command: TopologyCommand) {
        match command {
            TopologyCommand::ConnectPeer(addr) => {
                debug!(%addr, "Connect peer command - dial should be handled by swarm");
            }
            TopologyCommand::DisconnectPeer(peer_id) => {
                debug!(%peer_id, "Disconnect peer command");
                self.pending_actions.push_back(ToSwarm::CloseConnection {
                    peer_id,
                    connection: libp2p::swarm::CloseConnection::All,
                });
            }
            TopologyCommand::BroadcastPeers { to, peers } => {
                if let Some(connections) = self.peer_connections.get(&to) {
                    if let Some(&connection_id) = connections.first() {
                        for chunk in peers.chunks(MAX_BATCH_SIZE) {
                            self.pending_actions.push_back(ToSwarm::NotifyHandler {
                                peer_id: to,
                                handler: NotifyHandler::One(connection_id),
                                event: HandlerCommand::BroadcastPeers(Peers::new(chunk.to_vec())),
                            });
                        }
                    }
                }
            }
            TopologyCommand::BanPeer { peer_id, reason } => {
                debug!(%peer_id, ?reason, "Ban peer command");
                if let Some(info) = self.authenticated_peers.remove(&peer_id) {
                    self.overlay_to_peer.remove(&info.overlay);
                }
                self.pending_actions.push_back(ToSwarm::CloseConnection {
                    peer_id,
                    connection: libp2p::swarm::CloseConnection::All,
                });
            }
        }
    }

    /// Get information about an authenticated peer by peer_id.
    pub fn get_peer(&self, peer_id: &PeerId) -> Option<&PeerInfo> {
        self.authenticated_peers.get(peer_id)
    }

    /// Get information about an authenticated peer by overlay address.
    pub fn get_peer_by_overlay(&self, overlay: &OverlayAddress) -> Option<(&PeerId, &PeerInfo)> {
        self.overlay_to_peer.get(overlay).and_then(|peer_id| {
            self.authenticated_peers
                .get(peer_id)
                .map(|info| (peer_id, info))
        })
    }

    /// Get underlays for a discovered peer by overlay address.
    pub fn get_underlays(&self, overlay: &OverlayAddress) -> Option<&Vec<Multiaddr>> {
        self.underlay_cache.get(overlay)
    }

    /// Check if a peer is authenticated.
    pub fn is_authenticated(&self, peer_id: &PeerId) -> bool {
        self.authenticated_peers.contains_key(peer_id)
    }

    /// Get all authenticated peers.
    pub fn authenticated_peers(&self) -> impl Iterator<Item = (&PeerId, &PeerInfo)> {
        self.authenticated_peers.iter()
    }

    /// Broadcast peers to a connected peer.
    pub fn broadcast_peers(&mut self, to: PeerId, peers: Vec<BzzAddress>) {
        if let Some(connections) = self.peer_connections.get(&to) {
            if let Some(&connection_id) = connections.first() {
                for chunk in peers.chunks(MAX_BATCH_SIZE) {
                    self.pending_actions.push_back(ToSwarm::NotifyHandler {
                        peer_id: to,
                        handler: NotifyHandler::One(connection_id),
                        event: HandlerCommand::BroadcastPeers(Peers::new(chunk.to_vec())),
                    });
                }
            }
        }
    }

    /// Process a handler event.
    fn process_handler_event(&mut self, peer_id: PeerId, event: HandlerEvent) {
        match event {
            HandlerEvent::HandshakeCompleted(info) => {
                let overlay = OverlayAddress::new(info.ack.node_address().overlay_address().into());
                let is_full_node = info.ack.full_node();

                debug!(
                    %peer_id,
                    %overlay,
                    %is_full_node,
                    "Handshake completed, peer ready"
                );

                let peer_info = PeerInfo {
                    overlay,
                    is_full_node,
                    handshake: info,
                };
                self.authenticated_peers.insert(peer_id, peer_info);
                self.overlay_to_peer.insert(overlay, peer_id);

                self.pending_events.push_back(TopologyEvent::PeerReady {
                    peer_id,
                    overlay,
                    is_full_node,
                });
            }
            HandlerEvent::HandshakeFailed(error) => {
                warn!(%peer_id, %error, "Handshake failed");
                self.pending_events
                    .push_back(TopologyEvent::ConnectionFailed {
                        peer_id: Some(peer_id),
                        address: Multiaddr::empty(),
                        error: error.to_string(),
                    });
            }
            HandlerEvent::HivePeersReceived(peers) => {
                let discovered: Vec<(OverlayAddress, Vec<Multiaddr>)> = peers
                    .into_iter()
                    .map(|bzz| {
                        let overlay = OverlayAddress::from(bzz.overlay);
                        let underlays = bzz.underlays.clone();
                        // Cache the underlays for later dialing
                        self.underlay_cache.insert(overlay, bzz.underlays);
                        (overlay, underlays)
                    })
                    .collect();

                if !discovered.is_empty() {
                    debug!(%peer_id, count = discovered.len(), "Peers discovered via hive");
                    self.pending_events
                        .push_back(TopologyEvent::PeersDiscovered {
                            from: peer_id,
                            peers: discovered,
                        });
                }
            }
            HandlerEvent::HiveBroadcastComplete => {
                debug!(%peer_id, "Hive broadcast complete");
            }
            HandlerEvent::HiveError(error) => {
                warn!(%peer_id, %error, "Hive error");
            }
            HandlerEvent::PingpongPong { rtt } => {
                debug!(%peer_id, ?rtt, "Pingpong success");
            }
            HandlerEvent::PingpongPingReceived => {
                debug!(%peer_id, "Received ping from peer");
            }
            HandlerEvent::PingpongError(error) => {
                warn!(%peer_id, %error, "Pingpong failed");
            }
        }
    }
}

impl<N: NodeTypes> NetworkBehaviour for SwarmTopologyBehaviour<N> {
    type ConnectionHandler = TopologyHandler<N>;
    type ToSwarm = TopologyEvent;

    fn handle_established_inbound_connection(
        &mut self,
        _connection_id: ConnectionId,
        peer: PeerId,
        _local_addr: &Multiaddr,
        remote_addr: &Multiaddr,
    ) -> Result<Self::ConnectionHandler, ConnectionDenied> {
        Ok(TopologyHandler::new(
            self.config.handler.clone(),
            self.identity.clone(),
            peer,
            remote_addr,
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
            self.config.handler.clone(),
            self.identity.clone(),
            peer,
            addr,
        ))
    }

    fn on_swarm_event(&mut self, event: FromSwarm) {
        match event {
            FromSwarm::ConnectionEstablished(established) => {
                self.peer_connections
                    .entry(established.peer_id)
                    .or_default()
                    .push(established.connection_id);

                // For outbound connections, send StartHandshake command
                if established.endpoint.is_dialer() {
                    let resolved_addr = established.endpoint.get_remote_address().clone();
                    self.pending_actions.push_back(ToSwarm::NotifyHandler {
                        peer_id: established.peer_id,
                        handler: NotifyHandler::One(established.connection_id),
                        event: HandlerCommand::StartHandshake(resolved_addr),
                    });
                }
            }
            FromSwarm::ConnectionClosed(closed) => {
                if let Some(connections) = self.peer_connections.get_mut(&closed.peer_id) {
                    connections.retain(|&id| id != closed.connection_id);
                }
                if closed.remaining_established == 0 {
                    self.peer_connections.remove(&closed.peer_id);

                    if let Some(peer_info) = self.authenticated_peers.remove(&closed.peer_id) {
                        self.overlay_to_peer.remove(&peer_info.overlay);
                        debug!(
                            peer_id = %closed.peer_id,
                            overlay = %peer_info.overlay,
                            "Peer disconnected"
                        );
                        self.pending_events
                            .push_back(TopologyEvent::PeerDisconnected {
                                peer_id: closed.peer_id,
                                overlay: Some(peer_info.overlay),
                            });
                    }
                }
            }
            _ => {}
        }
    }

    fn on_connection_handler_event(
        &mut self,
        peer_id: PeerId,
        _connection_id: ConnectionId,
        event: HandlerEvent,
    ) {
        self.process_handler_event(peer_id, event);
    }

    fn poll(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        // Emit pending events first
        if let Some(event) = self.pending_events.pop_front() {
            return Poll::Ready(ToSwarm::GenerateEvent(event));
        }

        // Process pending actions
        if let Some(action) = self.pending_actions.pop_front() {
            return Poll::Ready(action);
        }

        Poll::Pending
    }
}
