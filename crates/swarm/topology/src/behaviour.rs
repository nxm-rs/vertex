//! Network topology behaviour.
//!
//! Manages handshake, hive, and pingpong protocols. Emits libp2p types only;
//! the client layer handles PeerId ↔ OverlayAddress mapping.

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
use vertex_net_hive::MAX_BATCH_SIZE;
use vertex_swarm_api::SwarmNodeTypes;
use vertex_swarm_peermanager::AddressManager;

use crate::{
    TopologyCommand, TopologyEvent,
    handler::{
        Command as HandlerCommand, Config as HandlerConfig, Event as HandlerEvent, TopologyHandler,
    },
};

/// Topology behaviour configuration.
#[derive(Debug, Clone, Default)]
pub struct Config {
    pub handler: HandlerConfig,
}

/// Network topology behaviour.
///
/// Handles handshake, hive, and pingpong. Maps handler events to `TopologyEvent`.
pub struct SwarmTopologyBehaviour<N: SwarmNodeTypes> {
    config: Config,
    identity: N::Identity,
    address_manager: Option<Arc<AddressManager>>,
    peer_connections: HashMap<PeerId, Vec<ConnectionId>>,
    pending_events: VecDeque<TopologyEvent>,
    pending_actions: VecDeque<ToSwarm<TopologyEvent, HandlerCommand>>,
}

impl<N: SwarmNodeTypes> SwarmTopologyBehaviour<N> {
    /// Creates a new topology behaviour.
    pub fn new(identity: N::Identity, config: Config) -> Self {
        Self {
            config,
            identity,
            address_manager: None,
            peer_connections: HashMap::new(),
            pending_events: VecDeque::new(),
            pending_actions: VecDeque::new(),
        }
    }

    /// Creates a topology behaviour with address management.
    pub fn with_address_manager(
        identity: N::Identity,
        config: Config,
        address_manager: Arc<AddressManager>,
    ) -> Self {
        Self {
            config,
            identity,
            address_manager: Some(address_manager),
            peer_connections: HashMap::new(),
            pending_events: VecDeque::new(),
            pending_actions: VecDeque::new(),
        }
    }

    /// Pings a peer.
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

    /// Handles a command from the client layer.
    pub fn on_command(&mut self, command: TopologyCommand) {
        match command {
            TopologyCommand::Dial(addr) => {
                debug!(%addr, "Dial command - dial should be handled by swarm");
            }
            TopologyCommand::CloseConnection(peer_id) => {
                debug!(%peer_id, "Close connection command");
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
                                event: HandlerCommand::BroadcastPeers(chunk.to_vec()),
                            });
                        }
                    }
                }
            }
        }
    }

    /// Returns true if connected to the peer.
    pub fn is_connected(&self, peer_id: &PeerId) -> bool {
        self.peer_connections
            .get(peer_id)
            .map(|conns| !conns.is_empty())
            .unwrap_or(false)
    }

    /// Returns all connected peer IDs.
    pub fn connected_peers(&self) -> impl Iterator<Item = &PeerId> {
        self.peer_connections
            .iter()
            .filter(|(_, conns)| !conns.is_empty())
            .map(|(peer_id, _)| peer_id)
    }

    /// Processes a handler event.
    fn process_handler_event(
        &mut self,
        peer_id: PeerId,
        connection_id: ConnectionId,
        event: HandlerEvent,
    ) {
        match event {
            HandlerEvent::HandshakeCompleted(info) => {
                debug!(
                    %peer_id,
                    %connection_id,
                    "Handshake completed, peer authenticated"
                );

                self.pending_events
                    .push_back(TopologyEvent::PeerAuthenticated {
                        peer_id,
                        connection_id,
                        info,
                    });
            }
            HandlerEvent::HandshakeFailed(error) => {
                warn!(%peer_id, %error, "Handshake failed");
                self.pending_events.push_back(TopologyEvent::DialFailed {
                    address: Multiaddr::empty(),
                    error: error.to_string(),
                });
            }
            HandlerEvent::HivePeersReceived(peers) => {
                if !peers.is_empty() {
                    debug!(%peer_id, count = peers.len(), "Peers received via hive");
                    self.pending_events
                        .push_back(TopologyEvent::HivePeersReceived {
                            from: peer_id,
                            peers,
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

impl<N: SwarmNodeTypes> NetworkBehaviour for SwarmTopologyBehaviour<N> {
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
                self.config.handler.clone(),
                self.identity.clone(),
                peer,
                remote_addr,
                mgr.clone(),
            ),
            None => TopologyHandler::new(
                self.config.handler.clone(),
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
                self.config.handler.clone(),
                self.identity.clone(),
                peer,
                addr,
                mgr.clone(),
            ),
            None => TopologyHandler::new(
                self.config.handler.clone(),
                self.identity.clone(),
                peer,
                addr,
            ),
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

                    debug!(
                        peer_id = %closed.peer_id,
                        "Peer connection closed"
                    );
                    self.pending_events
                        .push_back(TopologyEvent::PeerConnectionClosed {
                            peer_id: closed.peer_id,
                        });
                }
            }
            _ => {}
        }
    }

    fn on_connection_handler_event(
        &mut self,
        peer_id: PeerId,
        connection_id: ConnectionId,
        event: HandlerEvent,
    ) {
        self.process_handler_event(peer_id, connection_id, event);
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
