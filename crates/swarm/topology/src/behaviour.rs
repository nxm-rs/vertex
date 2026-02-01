//! Network topology behaviour.
//!
//! [`TopologyBehaviour`] is a libp2p `NetworkBehaviour` that manages peer connections
//! using handshake, hive, and pingpong protocols. It translates between the public
//! API ([`TopologyCommand`]/[`TopologyEvent`]) and per-connection handlers.

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
    handler::{Command, Event, TopologyConfig, TopologyHandler},
};

/// Network topology behaviour.
///
/// Manages peer connections across handshake, hive, and pingpong protocols.
/// Accepts [`TopologyCommand`]s via [`on_command`](Self::on_command) and emits
/// [`TopologyEvent`]s through the libp2p swarm.
pub struct TopologyBehaviour<N: SwarmNodeTypes> {
    config: TopologyConfig,
    identity: N::Identity,
    address_manager: Option<Arc<AddressManager>>,
    peer_connections: HashMap<PeerId, Vec<ConnectionId>>,
    pending_events: VecDeque<TopologyEvent>,
    pending_actions: VecDeque<ToSwarm<TopologyEvent, Command>>,
}

impl<N: SwarmNodeTypes> TopologyBehaviour<N> {
    /// Create a new topology behaviour.
    pub fn new(identity: N::Identity, config: TopologyConfig) -> Self {
        Self {
            config,
            identity,
            address_manager: None,
            peer_connections: HashMap::new(),
            pending_events: VecDeque::new(),
            pending_actions: VecDeque::new(),
        }
    }

    /// Create a topology behaviour with address management.
    pub fn with_address_manager(
        identity: N::Identity,
        config: TopologyConfig,
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

    /// Send a ping to a peer.
    pub fn ping(&mut self, peer_id: PeerId) {
        if let Some(connections) = self.peer_connections.get(&peer_id) {
            if let Some(&connection_id) = connections.first() {
                self.pending_actions.push_back(ToSwarm::NotifyHandler {
                    peer_id,
                    handler: NotifyHandler::One(connection_id),
                    event: Command::Ping { greeting: None },
                });
            }
        }
    }

    /// Handle a topology command.
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
                                event: Command::BroadcastPeers(chunk.to_vec()),
                            });
                        }
                    }
                }
            }
        }
    }

    /// Check if connected to a peer.
    pub fn is_connected(&self, peer_id: &PeerId) -> bool {
        self.peer_connections
            .get(peer_id)
            .map(|conns| !conns.is_empty())
            .unwrap_or(false)
    }

    /// Iterate over connected peer IDs.
    pub fn connected_peers(&self) -> impl Iterator<Item = &PeerId> {
        self.peer_connections
            .iter()
            .filter(|(_, conns)| !conns.is_empty())
            .map(|(peer_id, _)| peer_id)
    }

    fn process_handler_event(
        &mut self,
        peer_id: PeerId,
        connection_id: ConnectionId,
        event: Event,
    ) {
        match event {
            Event::HandshakeCompleted(info) => {
                debug!(%peer_id, %connection_id, "Handshake completed, peer authenticated");
                self.pending_events
                    .push_back(TopologyEvent::PeerAuthenticated {
                        peer_id,
                        connection_id,
                        info,
                    });
            }
            Event::HandshakeFailed(error) => {
                warn!(%peer_id, %error, "Handshake failed");
                self.pending_events.push_back(TopologyEvent::DialFailed {
                    address: Multiaddr::empty(),
                    error: error.to_string(),
                });
            }
            Event::HivePeersReceived(peers) => {
                if !peers.is_empty() {
                    debug!(%peer_id, count = peers.len(), "Peers received via hive");
                    self.pending_events
                        .push_back(TopologyEvent::HivePeersReceived {
                            from: peer_id,
                            peers,
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
            }
            Event::PingpongPingReceived => {
                debug!(%peer_id, "Received ping from peer");
            }
            Event::PingpongError(error) => {
                warn!(%peer_id, %error, "Pingpong failed");
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
                    debug!(peer_id = %closed.peer_id, "Peer connection closed");
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
        event: Event,
    ) {
        self.process_handler_event(peer_id, connection_id, event);
    }

    fn poll(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        if let Some(event) = self.pending_events.pop_front() {
            return Poll::Ready(ToSwarm::GenerateEvent(event));
        }

        if let Some(action) = self.pending_actions.pop_front() {
            return Poll::Ready(action);
        }

        Poll::Pending
    }
}
