// behaviour.rs
use std::{
    collections::{HashMap, VecDeque},
    task::{Context, Poll},
    time::Duration,
};

use libp2p::core::{Endpoint, Multiaddr, PeerId};
use libp2p::swarm::{
    ConnectionDenied, ConnectionHandler, ConnectionId, FromSwarm, NetworkBehaviour, NotifyHandler,
    ToSwarm,
};

use super::{
    handler::{Handler, HandlerEvent, HandlerIn},
    protocol::ProtocolConfig,
};

/// Configuration for the Handshake behaviour
#[derive(Debug, Clone)]
pub struct Config {
    /// Network ID for protocol version validation
    pub network_id: u64,
    /// Protocol configuration
    pub protocol_config: ProtocolConfig,
    /// Whether this node is a full node
    pub full_node: bool,
    /// Handshake timeout
    pub timeout: Duration,
    /// Optional welcome message
    pub welcome_message: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            network_id: 1,
            protocol_config: ProtocolConfig::new(crate::DEFAULT_PROTOCOL_NAME),
            full_node: true,
            timeout: Duration::from_secs(15),
            welcome_message: String::new(),
        }
    }
}

/// Events emitted by the Handshake behaviour
#[derive(Debug)]
pub enum Event {
    /// Handshake completed successfully with a peer
    Completed { peer: PeerId, info: HandshakeInfo },
    /// Handshake failed with a peer
    Failed {
        peer: PeerId,
        error: crate::HandshakeError,
    },
}

/// Information about a completed handshake
#[derive(Debug, Clone)]
pub struct HandshakeInfo {
    pub peer_id: PeerId,
    pub full_node: bool,
    pub welcome_message: String,
    pub observed_addrs: Vec<Multiaddr>,
}

/// Tracks handshake state for a peer
#[derive(Debug)]
struct PeerState {
    info: HandshakeInfo,
    connections: Vec<ConnectionId>,
}

pub struct Behaviour {
    /// Configuration options
    config: Config,
    /// Completed handshakes by peer
    peers: HashMap<PeerId, PeerState>,
    /// Pending events to emit
    events: VecDeque<ToSwarm<Event, HandlerIn>>,
}

impl Behaviour {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            peers: HashMap::new(),
            events: VecDeque::new(),
        }
    }

    /// Get handshake info for a peer if available
    pub fn peer_info(&self, peer: &PeerId) -> Option<&HandshakeInfo> {
        self.peers.get(peer).map(|state| &state.info)
    }

    /// Check if we have completed a handshake with a peer
    pub fn is_peer_handshaked(&self, peer: &PeerId) -> bool {
        self.peers.contains_key(peer)
    }
}

impl NetworkBehaviour for Behaviour {
    type ConnectionHandler = Handler;
    type ToSwarm = Event;

    fn handle_established_inbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: PeerId,
        local_addr: &Multiaddr,
        remote_addr: &Multiaddr,
    ) -> Result<Self::ConnectionHandler, ConnectionDenied> {
        Ok(Handler::new(
            self.config.clone(),
            remote_addr.clone(),
            Endpoint::Listener,
        ))
    }

    fn handle_established_outbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: PeerId,
        addr: &Multiaddr,
        role_override: Endpoint,
    ) -> Result<Self::ConnectionHandler, ConnectionDenied> {
        Ok(Handler::new(
            self.config.clone(),
            addr.clone(),
            Endpoint::Dialer,
        ))
    }

    fn on_swarm_event(&mut self, event: FromSwarm) {
        match event {
            FromSwarm::ConnectionEstablished(connection) => {
                // Start handshake when connection established
                self.events.push_back(ToSwarm::NotifyHandler {
                    peer_id: connection.peer_id,
                    handler: NotifyHandler::One(connection.connection_id),
                    event: HandlerIn::StartHandshake,
                });
            }
            FromSwarm::ConnectionClosed(connection) => {
                // Remove connection from peer state
                if let Some(state) = self.peers.get_mut(&connection.peer_id) {
                    state.connections.retain(|c| *c != connection.connection_id);
                    if state.connections.is_empty() {
                        self.peers.remove(&connection.peer_id);
                    }
                }
            }
            _ => {}
        }
    }

    fn on_connection_handler_event(
        &mut self,
        peer_id: PeerId,
        connection: ConnectionId,
        event: HandlerEvent,
    ) {
        match event {
            HandlerEvent::HandshakeCompleted(info) => {
                // Store completed handshake
                self.peers
                    .entry(peer_id)
                    .and_modify(|state| {
                        if !state.connections.contains(&connection) {
                            state.connections.push(connection);
                        }
                    })
                    .or_insert_with(|| PeerState {
                        info: info.clone(),
                        connections: vec![connection],
                    });

                self.events
                    .push_back(ToSwarm::GenerateEvent(Event::Completed {
                        peer: peer_id,
                        info,
                    }));
            }
            HandlerEvent::HandshakeFailed(error) => {
                self.events.push_back(ToSwarm::GenerateEvent(Event::Failed {
                    peer: peer_id,
                    error,
                }));
            }
        }
    }

    fn poll(&mut self, cx: &mut Context<'_>) -> Poll<ToSwarm<Self::ToSwarm, HandlerIn>> {
        if let Some(event) = self.events.pop_front() {
            return Poll::Ready(event);
        }
        Poll::Pending
    }
}
